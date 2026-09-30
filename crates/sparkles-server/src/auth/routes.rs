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
    ("/$/auth/whoami", &["GET"]),
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
        "/" | "/ui" | "/ui/" | "/ui/{*path}" | "/$/ping" | "/$/ready" | "/$/auth/whoami" => Public,
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
        r if r.starts_with("/$/auth/") => super_auth_need(r)?,
        _ => return None,
    })
}

/// Needs of the `/$/auth/…` routes (token management and login flows).
fn super_auth_need(route: &str) -> Option<Need> {
    AUTH_ROUTES
        .iter()
        .find(|(r, _, _)| *r == route)
        .map(|(_, _, n)| *n)
}

/// `/$/auth/…` routes: template, methods, need.
pub const AUTH_ROUTES: &[(&str, &[&str], Need)] = &[];

/// The request's own origin, from `X-Forwarded-Proto` (if any) and `Host`.
fn same_origin(origin: &str, h: &HeaderMap) -> bool {
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
/// an `Origin` that is neither the request's own nor an allowed CORS origin.
fn cross_origin(h: &HeaderMap, allowed: &[String]) -> bool {
    if h.get("sec-fetch-site").is_some_and(|v| v == "cross-site") {
        return true;
    }
    match h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        None => false,
        Some(o) => !same_origin(o, h) && !allowed.iter().any(|a| a.eq_ignore_ascii_case(o)),
    }
}

/// Why the auth layer refused a request (metric label `kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Denied {
    Unauthenticated,
    Forbidden,
    Hidden,
    CrossOrigin,
}

impl Denied {
    pub const ALL: [Denied; 4] = [
        Denied::Unauthenticated,
        Denied::Forbidden,
        Denied::Hidden,
        Denied::CrossOrigin,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Denied::Unauthenticated => "unauthenticated",
            Denied::Forbidden => "forbidden",
            Denied::Hidden => "hidden",
            Denied::CrossOrigin => "cross_origin",
        }
    }
}

/// What the auth layer reports to `obs` through the response extensions.
#[derive(Clone, Debug, Default)]
pub struct AuthReport {
    /// log name of the principal (`None`: auth disabled, or authentication failed)
    pub principal: Option<String>,
    pub scheme: Option<&'static str>,
    pub denied: Option<Denied>,
    /// `invalid`, `expired`, `malformed` or `busy`
    pub error: Option<&'static str>,
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(json!({ "error": msg }))).into_response()
}

/// `WWW-Authenticate` challenges of a 401. `Basic` is left out for script fetches, so
/// browsers do not show their login dialog over the UI.
fn challenges(realm: &str, h: &HeaderMap, bearer_error: Option<&str>) -> Vec<HeaderValue> {
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
    if !script {
        out.extend(
            HeaderValue::from_str(&format!("Basic realm=\"{realm}\", charset=\"UTF-8\"")).ok(),
        );
    }
    out
}

fn unauthorized(realm: &str, h: &HeaderMap, msg: &str, bearer_error: Option<&str>) -> Response {
    let mut r = json_error(StatusCode::UNAUTHORIZED, msg);
    for c in challenges(realm, h, bearer_error) {
        r.headers_mut().append(header::WWW_AUTHENTICATE, c);
    }
    r
}

fn with_report(mut r: Response, report: AuthReport) -> Response {
    r.extensions_mut().insert(report);
    r
}

/// A 403 from a handler (the form-POST re-check, the clone target check).
pub fn forbidden(p: &Principal, msg: &str) -> Response {
    let mut r = json_error(StatusCode::FORBIDDEN, msg);
    if p.scheme == Scheme::Bearer
        && let Ok(v) = HeaderValue::from_str("Bearer error=\"insufficient_scope\"")
    {
        r.headers_mut().insert(header::WWW_AUTHENTICATE, v);
    }
    with_report(
        r,
        AuthReport {
            principal: p.log_name(),
            scheme: Some(p.scheme.as_str()),
            denied: Some(Denied::Forbidden),
            error: None,
        },
    )
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
    let deny = |kind: Denied, r: Response, p: Option<&Principal>| {
        auth.metrics.denied[Denied::ALL.iter().position(|d| *d == kind).unwrap_or(0)]
            .fetch_add(1, Ordering::Relaxed);
        with_report(
            r,
            AuthReport {
                principal: p.and_then(Principal::log_name),
                scheme: p.map(|p| p.scheme.as_str()),
                denied: Some(kind),
                error: None,
            },
        )
    };

    // 1. authenticate
    let p = match auth.authenticate(req.headers()).await {
        Ok(p) => p,
        Err(e) => {
            auth.count_failure(e);
            let span = tracing::Span::current();
            span.record("principal", "-");
            let r = match e.failure {
                Failure::Busy => {
                    let mut r = json_error(StatusCode::SERVICE_UNAVAILABLE, "authentication busy");
                    r.headers_mut()
                        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                    r
                }
                Failure::Expired => {
                    unauthorized(realm, req.headers(), "token expired", Some("token expired"))
                }
                _ => unauthorized(
                    realm,
                    req.headers(),
                    "invalid credentials",
                    Some("invalid credentials"),
                ),
            };
            return with_report(
                r,
                AuthReport {
                    principal: None,
                    scheme: Some(e.scheme.as_str()),
                    denied: None,
                    error: Some(e.failure.as_str()),
                },
            );
        }
    };
    if let Some(n) = p.log_name() {
        tracing::Span::current().record("principal", n.as_str());
    }

    // 2. what the route needs
    let method = req.method().clone();
    let need = need(&route, &method, req.uri(), req.headers())
        .unwrap_or(Need::Server(ServerPerm::ServerAdmin));

    // 3. CSRF gate
    let gated = !safe(&method)
        || matches!(
            need,
            Need::Dataset(Level::Write | Level::Admin) | Need::Server(ServerPerm::ServerAdmin)
        );
    if gated && cross_origin(req.headers(), &policy.cors_origins) {
        return deny(
            Denied::CrossOrigin,
            json_error(StatusCode::FORBIDDEN, "cross-origin request refused"),
            Some(&p),
        );
    }

    // 4./5. decide
    let unauth = |h: &HeaderMap| unauthorized(realm, h, "authentication required", None);
    match need {
        Need::Public | Need::Caller => {}
        Need::Server(perm) => {
            if !p.has(perm) {
                if p.is_anonymous() {
                    return deny(Denied::Unauthenticated, unauth(req.headers()), Some(&p));
                }
                let msg = format!("{} permission required", perm.as_str());
                return deny(Denied::Forbidden, forbidden(&p, &msg), Some(&p));
            }
        }
        Need::Dataset(lvl) => {
            let ds = ds_of(&route, req.uri()).unwrap_or_default();
            let have = p.level(&ds);
            if have.is_none_or(|h| h < lvl) {
                if p.is_anonymous() {
                    return deny(Denied::Unauthenticated, unauth(req.headers()), Some(&p));
                }
                if have.is_none() || st.get(&ds).is_none() {
                    // the same body as the handlers' own 404
                    let msg = format!("no such dataset: /{ds}");
                    return deny(
                        Denied::Hidden,
                        json_error(StatusCode::NOT_FOUND, &msg),
                        Some(&p),
                    );
                }
                let msg = format!("{} access to /{ds} required", lvl.as_str());
                return deny(Denied::Forbidden, forbidden(&p, &msg), Some(&p));
            }
        }
    }

    // 6. pass
    let report = AuthReport {
        principal: p.log_name(),
        scheme: Some(p.scheme.as_str()),
        denied: None,
        error: None,
    };
    req.extensions_mut().insert(p);
    let mut resp = next.run(req).await;
    if route.starts_with("/$/") && !resp.headers().contains_key(header::CACHE_CONTROL) {
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    // a handler's own denial (re-check, target check) keeps its report
    if !resp
        .extensions()
        .get::<AuthReport>()
        .is_some_and(|r| r.denied.is_some())
    {
        resp.extensions_mut().insert(report);
    }
    resp
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
        assert!(!cross_origin(&h(&[("host", "a:3030")]), &allowed));
        assert!(!cross_origin(
            &h(&[("host", "a:3030"), ("origin", "http://a:3030")]),
            &allowed
        ));
        assert!(cross_origin(
            &h(&[("host", "a:3030"), ("origin", "https://evil.example")]),
            &allowed
        ));
        assert!(!cross_origin(
            &h(&[("host", "a:3030"), ("origin", "https://yasgui.example")]),
            &allowed
        ));
        assert!(cross_origin(
            &h(&[
                ("host", "a"),
                ("origin", "http://a"),
                ("x-forwarded-proto", "https")
            ]),
            &allowed
        ));
        assert!(cross_origin(
            &h(&[("host", "a:3030"), ("sec-fetch-site", "cross-site")]),
            &allowed
        ));
        assert!(!cross_origin(
            &h(&[("host", "a:3030"), ("sec-fetch-site", "same-origin")]),
            &allowed
        ));
    }
}
