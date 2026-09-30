//! The route table and the authorization middleware.

use super::{Level, Principal, Scheme, ServerPerm};
use crate::state::AppState;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

/// What a route requires of its caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Need {
    /// no check (invalid credentials are still rejected)
    Public,
    /// any caller, anonymous included
    Caller,
    /// any principal except anonymous
    Authed,
    /// a session or proxy principal (a person in a browser)
    Interactive,
    /// that level on the route's `{ds}`
    Dataset(Level),
    Server(ServerPerm),
}

/// Every route template of the router with the methods it serves. A router test checks
/// that each registered template is listed; [`need`] answers `server-admin` for any
/// template it does not know (fail closed).
#[cfg_attr(not(test), allow(dead_code))]
pub const ROUTES: &[(&str, &[&str])] = &[
    ("/", &["GET"]),
    ("/ui", &["GET"]),
    ("/ui/", &["GET"]),
    ("/ui/{*path}", &["GET"]),
    ("/$/ping", &["GET", "POST"]),
    ("/$/whoami", &["GET"]),
    ("/$/server", &["GET"]),
    ("/$/metrics", &["GET"]),
    ("/$/ready", &["GET"]),
    ("/$/ready/{ds}", &["GET"]),
    ("/$/datasets", &["GET", "POST"]),
    ("/$/datasets/{ds}", &["GET", "DELETE"]),
    ("/$/datasets/{ds}/clone", &["POST"]),
    ("/$/stats/{ds}", &["GET"]),
    ("/$/schema/{ds}", &["GET"]),
    ("/$/schema/{ds}/classes", &["GET"]),
    ("/$/schema/{ds}/predicates", &["GET"]),
    ("/$/compact/{ds}", &["POST"]),
    ("/$/backup/{ds}", &["POST"]),
    ("/$/reason/{ds}", &["GET", "POST", "DELETE"]),
    ("/$/reason/{ds}/diagnostics", &["GET"]),
    ("/$/tasks", &["GET"]),
    ("/$/tasks/{id}", &["GET"]),
    ("/$/prefixes/{ds}", &["GET"]),
    ("/$/cache/clear/{ds}", &["POST"]),
    ("/$/text/{ds}", &["GET", "PUT", "DELETE"]),
    ("/$/text/{ds}/rebuild", &["POST"]),
    ("/$/commits/{ds}", &["GET"]),
    ("/$/commits/{ds}/{reference}", &["GET"]),
    ("/$/auth/config", &["GET"]),
    ("/$/auth/login", &["POST"]),
    ("/$/auth/logout", &["POST"]),
    ("/$/auth/oidc/login", &["GET"]),
    ("/$/auth/oidc/callback", &["GET"]),
    ("/$/auth/tokens", &["GET", "POST", "DELETE"]),
    ("/$/auth/tokens/{id}", &["DELETE"]),
    ("/$/auth/device", &["POST"]),
    ("/$/auth/device/{user_code}", &["GET"]),
    ("/$/auth/device/{user_code}/approve", &["POST"]),
    ("/$/auth/device/{user_code}/deny", &["POST"]),
    ("/$/auth/cli/authorize", &["POST"]),
    ("/$/auth/token", &["POST"]),
    ("/{ds}", &["*"]),
    ("/{ds}/sparql", &["*"]),
    ("/{ds}/query", &["*"]),
    ("/{ds}/update", &["POST"]),
    ("/{ds}/data", &["*"]),
    ("/{ds}/get", &["GET", "HEAD"]),
    ("/{ds}/upload", &["POST"]),
    ("/{ds}/explain", &["GET", "POST"]),
    ("/{ds}/shacl", &["POST"]),
];

/// Routes of the CLI grants: they identify the client by a device code or a PKCE
/// verifier, never by cookies, so they need no CSRF token.
fn cli_grant_route(route: &str) -> bool {
    matches!(route, "/$/auth/device" | "/$/auth/token")
}

fn safe(m: &Method) -> bool {
    matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn has_param(uri: &Uri, k: &str) -> bool {
    uri.query()
        .is_some_and(|q| form_urlencoded::parse(q.as_bytes()).any(|(a, _)| a == k))
}

fn media_type(h: &HeaderMap) -> String {
    h.get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

/// The permission a request to `route` needs; `None` for a template not in the table.
pub fn need(route: &str, method: &Method, uri: &Uri, headers: &HeaderMap) -> Option<Need> {
    use Level::*;
    use Need::*;
    let get = matches!(*method, Method::GET | Method::HEAD);
    Some(match route {
        "/" | "/ui" | "/ui/" | "/ui/{*path}" | "/$/ping" | "/$/ready" | "/$/whoami" => Public,
        "/$/auth/config"
        | "/$/auth/login"
        | "/$/auth/oidc/login"
        | "/$/auth/oidc/callback"
        | "/$/auth/device"
        | "/$/auth/token" => Public,
        "/$/auth/logout" => Caller,
        "/$/auth/tokens" if *method == Method::DELETE => Server(ServerPerm::ServerAdmin),
        "/$/auth/tokens" | "/$/auth/tokens/{id}" => Authed,
        "/$/auth/device/{user_code}"
        | "/$/auth/device/{user_code}/approve"
        | "/$/auth/device/{user_code}/deny"
        | "/$/auth/cli/authorize" => Interactive,
        "/$/server" | "/$/tasks" | "/$/tasks/{id}" => Caller,
        "/$/metrics" => Server(ServerPerm::Metrics),
        "/$/datasets" if get => Caller,
        "/$/datasets" => Server(ServerPerm::ServerAdmin),
        "/$/datasets/{ds}" if get => Dataset(Read),
        "/$/datasets/{ds}" => Dataset(Admin),
        "/$/ready/{ds}"
        | "/$/stats/{ds}"
        | "/$/schema/{ds}"
        | "/$/schema/{ds}/classes"
        | "/$/schema/{ds}/predicates"
        | "/$/reason/{ds}/diagnostics"
        | "/$/prefixes/{ds}"
        | "/$/commits/{ds}"
        | "/$/commits/{ds}/{reference}"
        | "/{ds}/sparql"
        | "/{ds}/query"
        | "/{ds}/explain"
        | "/{ds}/get"
        | "/{ds}/shacl" => Dataset(Read),
        "/$/reason/{ds}" | "/$/text/{ds}" if get => Dataset(Read),
        "/$/reason/{ds}"
        | "/$/text/{ds}"
        | "/$/text/{ds}/rebuild"
        | "/$/datasets/{ds}/clone"
        | "/$/compact/{ds}"
        | "/$/backup/{ds}"
        | "/$/cache/clear/{ds}" => Dataset(Admin),
        "/{ds}/update" | "/{ds}/upload" => Dataset(Write),
        "/{ds}/data" if get => Dataset(Read),
        "/{ds}/data" => Dataset(Write),
        "/{ds}" => {
            let ct = media_type(headers);
            if has_param(uri, "update") || ct == "application/sparql-update" {
                Dataset(Write)
            } else if has_param(uri, "query")
                || ct == "application/sparql-query"
                || safe(method)
                // the body may hold `update=`: `dataset_root` re-checks write
                || (*method == Method::POST && ct == "application/x-www-form-urlencoded")
            {
                Dataset(Read)
            } else {
                Dataset(Write)
            }
        }
        _ => return None,
    })
}

/// The origin of `public_url`, or else the request's own (`X-Forwarded-Proto` or either
/// scheme, and `Host`).
fn own_origin(origin: &str, h: &HeaderMap, public_url: Option<&str>) -> bool {
    if let Some(u) = public_url {
        return origin.eq_ignore_ascii_case(u.trim_end_matches('/'));
    }
    let Some(host) = h.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    if !rest.eq_ignore_ascii_case(host) {
        return false;
    }
    match h
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|p| {
            p.split(',')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        }) {
        Some(p) => p == scheme,
        // without the header the scheme is unknown (TLS may end at a proxy)
        None => scheme == "http" || scheme == "https",
    }
}

/// OWASP's origin verification with standard headers: a cross-site `Sec-Fetch-Site`, or
/// an `Origin` that is neither the server's own nor an allowed CORS origin.
fn cross_origin(h: &HeaderMap, allowed: &[String], public_url: Option<&str>) -> bool {
    if h.get("sec-fetch-site").is_some_and(|v| v == "cross-site") {
        return true;
    }
    match h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => false,
        Some(o) => {
            !own_origin(o, h, public_url) && !allowed.iter().any(|a| a.eq_ignore_ascii_case(o))
        }
    }
}

/// Why the auth layer refused a request (metric label `kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    Unauthenticated,
    Forbidden,
    Hidden,
    CrossOrigin,
    Csrf,
    NotInteractive,
}

impl Denied {
    pub const ALL: [Denied; 6] = [
        Denied::Unauthenticated,
        Denied::Forbidden,
        Denied::Hidden,
        Denied::CrossOrigin,
        Denied::Csrf,
        Denied::NotInteractive,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Denied::Unauthenticated => "unauthenticated",
            Denied::Forbidden => "forbidden",
            Denied::Hidden => "hidden",
            Denied::CrossOrigin => "cross_origin",
            Denied::Csrf => "csrf",
            Denied::NotInteractive => "not_interactive",
        }
    }
}

/// What the auth layer reports to `obs` through the response extensions.
#[derive(Clone, Debug, Default)]
pub struct AuthReport {
    /// log name of the principal (`None`: auth disabled, or authentication failed)
    pub principal: Option<String>,
    /// `none`, `basic`, `bearer`, `session` or `proxy`
    pub scheme: Option<&'static str>,
    pub denied: Option<Denied>,
    /// `invalid`, `expired`, `malformed`, `busy`, `not_allowed`
    pub error: Option<&'static str>,
}

pub fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(json!({ "error": msg }))).into_response()
}

/// `WWW-Authenticate` challenges of a 401. `Basic` is left out for script fetches (no
/// browser login dialog over the UI) and when no user can sign in with a password.
fn challenges(
    realm: &str,
    h: &HeaderMap,
    bearer_error: Option<&str>,
    basic: bool,
) -> Vec<HeaderValue> {
    let mut out = Vec::new();
    let bearer = match bearer_error {
        None => format!("Bearer realm=\"{realm}\""),
        Some(d) => {
            format!("Bearer realm=\"{realm}\", error=\"invalid_token\", error_description=\"{d}\"")
        }
    };
    out.extend(HeaderValue::from_str(&bearer).ok());
    let script = h
        .get("sec-fetch-mode")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|m| matches!(m, "cors" | "same-origin" | "no-cors"));
    if basic && !script {
        out.extend(
            HeaderValue::from_str(&format!("Basic realm=\"{realm}\", charset=\"UTF-8\"")).ok(),
        );
    }
    out
}

fn unauthorized(
    realm: &str,
    h: &HeaderMap,
    msg: &str,
    bearer_error: Option<&str>,
    basic: bool,
) -> Response {
    let mut r = json_error(StatusCode::UNAUTHORIZED, msg);
    for c in challenges(realm, h, bearer_error, basic) {
        r.headers_mut().append(header::WWW_AUTHENTICATE, c);
    }
    r
}

fn with_report(mut r: Response, report: AuthReport) -> Response {
    r.extensions_mut().insert(report);
    r
}

/// A 403 (from the middleware, or a handler's own check).
pub fn forbidden(p: &Principal, msg: &str) -> Response {
    forbidden_as(p, msg, Denied::Forbidden)
}

fn forbidden_as(p: &Principal, msg: &str, kind: Denied) -> Response {
    let mut r = json_error(StatusCode::FORBIDDEN, msg);
    if p.scheme == Scheme::Bearer
        && let Ok(v) = HeaderValue::from_str("Bearer error=\"insufficient_scope\"")
    {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    with_report(r, report_of(p, Some(kind)))
}

fn report_of(p: &Principal, denied: Option<Denied>) -> AuthReport {
    AuthReport {
        principal: p.log_name(),
        scheme: Some(p.scheme.as_str()),
        denied,
        error: None,
    }
}

/// The `{ds}` segment of the request path.
fn ds_of(route: &str, uri: &Uri) -> Option<String> {
    let pos = route.split('/').position(|s| s == "{ds}")?;
    let seg = uri.path().split('/').nth(pos)?;
    Some(
        percent_encoding::percent_decode_str(seg)
            .decode_utf8_lossy()
            .into_owned(),
    )
}

/// The CSRF header of ambient principals.
pub const CSRF_HEADER: &str = "x-sparkles-csrf";

/// Authenticate, then authorize against the route table (see the module docs).
pub async fn middleware(State(st): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    #[cfg(feature = "auth")]
    if let Some(auth) = st.auth.clone() {
        return enforce(&st, &auth, req, next).await;
    }
    let _ = &st;
    req.extensions_mut().insert(Principal::local());
    next.run(req).await
}

#[cfg(feature = "auth")]
async fn enforce(st: &AppState, auth: &super::Auth, mut req: Request, next: Next) -> Response {
    use super::policy::Failure;
    use std::sync::atomic::Ordering;
    let Some(route) = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
    else {
        // unmatched: the router's 404, no dataset involved
        return next.run(req).await;
    };
    let policy = auth.policy();
    let realm = policy.realm.as_str();
    let basic = policy.has_users();
    let span = tracing::Span::current();
    let count = |kind: Denied| {
        auth.metrics.denied[Denied::ALL.iter().position(|d| *d == kind).unwrap_or(0)]
            .fetch_add(1, Ordering::Relaxed);
    };
    let deny = |kind: Denied, r: Response, p: &Principal| {
        count(kind);
        with_report(r, report_of(p, Some(kind)))
    };

    // 1. peer, 2. authenticate
    let peer = req
        .extensions()
        .get::<axum::extract::ConnectInfo<super::Peer>>()
        .map(|c| c.0);
    let authed = match auth.authenticate(req.headers(), peer.as_ref()).await {
        Ok(a) => a,
        Err(e) => {
            auth.count_failure(e);
            span.record("principal", "-");
            span.record("auth", e.scheme);
            let r = match e.failure {
                Failure::Busy => {
                    let mut r = json_error(StatusCode::SERVICE_UNAVAILABLE, "authentication busy");
                    r.headers_mut()
                        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                    r
                }
                Failure::NotAllowed => {
                    if matches!(
                        need(&route, req.method(), req.uri(), req.headers()),
                        Some(Need::Public)
                    ) {
                        // public routes stay reachable (the UI shell, health, login)
                        let a = auth.anonymous_principal();
                        span.record("principal", "anonymous");
                        req.extensions_mut().insert(a);
                        return next.run(req).await;
                    }
                    count(Denied::Forbidden);
                    json_error(StatusCode::FORBIDDEN, "user not allowed")
                }
                Failure::Expired => unauthorized(
                    realm,
                    req.headers(),
                    "token expired",
                    Some("token expired"),
                    basic,
                ),
                _ => unauthorized(
                    realm,
                    req.headers(),
                    "invalid credentials",
                    Some("invalid credentials"),
                    basic,
                ),
            };
            return with_report(
                r,
                AuthReport {
                    principal: None,
                    scheme: Some(e.scheme),
                    denied: None,
                    error: Some(e.failure.as_str()),
                },
            );
        }
    };
    let p = authed.principal;
    let clear = authed.clear_cookie.then(|| {
        auth.cookie_mode(req.headers())
            .set(super::SESSION_COOKIE, "", 0)
    });
    let finish = |mut r: Response| {
        if let Some(c) = &clear {
            r.headers_mut().append(header::SET_COOKIE, c.clone());
        }
        r
    };
    if let Some(n) = p.log_name() {
        span.record("principal", n.as_str());
    }
    span.record("auth", p.scheme.as_str());

    // what the route needs
    let method = req.method().clone();
    let need = need(&route, &method, req.uri(), req.headers())
        .unwrap_or(Need::Server(ServerPerm::ServerAdmin));

    // 3. CSRF gates: (a) the origin, (b) the synchronizer token of ambient principals
    let gated = !safe(&method)
        || matches!(
            need,
            Need::Dataset(Level::Write | Level::Admin) | Need::Server(ServerPerm::ServerAdmin)
        );
    if gated
        && cross_origin(
            req.headers(),
            &policy.cors_origins,
            policy.public_url.as_deref(),
        )
    {
        let r = json_error(StatusCode::FORBIDDEN, "cross-origin request refused");
        return finish(deny(Denied::CrossOrigin, r, &p));
    }
    if !safe(&method) && p.is_ambient() && !cli_grant_route(&route) {
        let sent = req
            .headers()
            .get(CSRF_HEADER)
            .map(|v| v.as_bytes())
            .unwrap_or_default();
        let want = p.info.csrf.as_deref().unwrap_or("");
        if want.is_empty() || !super::crypto::ct_eq(sent, want.as_bytes()) {
            let r = json_error(StatusCode::FORBIDDEN, "CSRF token missing or invalid");
            return finish(deny(Denied::Csrf, r, &p));
        }
    }

    // 4.–6. decide
    let unauth = |h: &HeaderMap| unauthorized(realm, h, "authentication required", None, basic);
    match need {
        Need::Public | Need::Caller => {}
        Need::Authed => {
            if p.is_anonymous() {
                return finish(deny(Denied::Unauthenticated, unauth(req.headers()), &p));
            }
        }
        Need::Interactive => {
            if p.is_anonymous() {
                return finish(deny(Denied::Unauthenticated, unauth(req.headers()), &p));
            }
            if !p.is_interactive() {
                count(Denied::NotInteractive);
                return finish(forbidden_as(
                    &p,
                    "this action requires signing in to the web UI",
                    Denied::NotInteractive,
                ));
            }
        }
        Need::Server(perm) => {
            if !p.has(perm) {
                if p.is_anonymous() {
                    return finish(deny(Denied::Unauthenticated, unauth(req.headers()), &p));
                }
                count(Denied::Forbidden);
                let msg = format!("{} permission required", perm.as_str());
                return finish(forbidden(&p, &msg));
            }
        }
        Need::Dataset(lvl) => {
            let ds = ds_of(&route, req.uri()).unwrap_or_default();
            let have = p.level(&ds);
            if have.is_none_or(|h| h < lvl) {
                if p.is_anonymous() {
                    return finish(deny(Denied::Unauthenticated, unauth(req.headers()), &p));
                }
                if have.is_none() || st.get(&ds).is_none() {
                    // the same body as the handlers' own 404
                    let msg = format!("no such dataset: /{ds}");
                    let r = json_error(StatusCode::NOT_FOUND, &msg);
                    return finish(deny(Denied::Hidden, r, &p));
                }
                count(Denied::Forbidden);
                let msg = format!("{} access to /{ds} required", lvl.as_str());
                return finish(forbidden(&p, &msg));
            }
        }
    }

    // pass
    let report = report_of(&p, None);
    req.extensions_mut().insert(p);
    let mut resp = next.run(req).await;
    if route.starts_with("/$/") && !resp.headers().contains_key(header::CACHE_CONTROL) {
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    // a handler's own denial (re-check, target check) keeps its report
    let denied = resp.extensions().get::<AuthReport>().and_then(|r| r.denied);
    match denied {
        Some(kind) => count(kind),
        None => {
            resp.extensions_mut().insert(report);
        }
    }
    finish(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        m
    }

    #[test]
    fn dataset_root_classification() {
        let n = |m: Method, uri: &str, ct: &str| {
            need(
                "/{ds}",
                &m,
                &uri.parse().unwrap(),
                &h(&[("content-type", ct)]),
            )
            .unwrap()
        };
        let r = Need::Dataset(Level::Read);
        let w = Need::Dataset(Level::Write);
        assert_eq!(n(Method::GET, "/ds?query=x", ""), r);
        assert_eq!(n(Method::GET, "/ds?update=x", ""), w);
        assert_eq!(n(Method::POST, "/ds", "application/sparql-update"), w);
        assert_eq!(n(Method::POST, "/ds", "application/sparql-query"), r);
        assert_eq!(n(Method::GET, "/ds", ""), r);
        assert_eq!(
            n(Method::POST, "/ds", "application/x-www-form-urlencoded"),
            r
        );
        assert_eq!(n(Method::POST, "/ds", "text/turtle"), w);
        assert_eq!(n(Method::PUT, "/ds", "text/turtle"), w);
        assert_eq!(n(Method::DELETE, "/ds", ""), w);
    }

    #[test]
    fn origins() {
        let allowed = vec!["https://yasgui.example".to_string()];
        let x = |pairs: &[(&str, &str)]| cross_origin(&h(pairs), &allowed, None);
        assert!(!x(&[("host", "a:3030")]));
        assert!(!x(&[("host", "a:3030"), ("origin", "http://a:3030")]));
        assert!(x(&[("host", "a:3030"), ("origin", "https://evil.example")]));
        assert!(!x(&[
            ("host", "a:3030"),
            ("origin", "https://yasgui.example")
        ]));
        assert!(x(&[
            ("host", "a"),
            ("origin", "http://a"),
            ("x-forwarded-proto", "https")
        ]));
        assert!(x(&[("host", "a:3030"), ("sec-fetch-site", "cross-site")]));
        assert!(!x(&[("host", "a:3030"), ("sec-fetch-site", "same-origin")]));
        // with a public URL, Host does not matter
        let pu = Some("https://sparql.example.org");
        assert!(!cross_origin(
            &h(&[("host", "evil"), ("origin", "https://sparql.example.org")]),
            &allowed,
            pu
        ));
        assert!(cross_origin(
            &h(&[("host", "a:3030"), ("origin", "http://a:3030")]),
            &allowed,
            pu
        ));
    }
}
