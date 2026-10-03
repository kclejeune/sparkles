//! The route table and the authorization middleware.

use super::{Endpoint, Level, Principal, Scheme, ServerPerm};
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
    ("/$/server", &["GET", "POST"]),
    ("/$/metrics", &["GET"]),
    ("/$/ready", &["GET"]),
    ("/$/ready/{ds}", &["GET"]),
    ("/$/datasets", &["GET", "POST"]),
    // POST: Fuseki's `?state=offline|active`
    ("/$/datasets/{ds}", &["GET", "POST", "DELETE"]),
    ("/$/datasets/{ds}/clone", &["POST"]),
    ("/$/stats/{ds}", &["GET", "POST"]),
    // Fuseki's routes (`http/fuseki.rs`)
    ("/$/stats", &["GET", "POST"]),
    ("/$/backups-list", &["GET", "POST"]),
    ("/$/validate/query", &["GET", "POST"]),
    ("/$/validate/update", &["GET", "POST"]),
    ("/$/validate/iri", &["GET", "POST"]),
    ("/$/validate/data", &["GET", "POST"]),
    ("/$/validate/langtag", &["GET", "POST"]),
    ("/$/schema/{ds}", &["GET"]),
    ("/$/schema/{ds}/classes", &["GET"]),
    ("/$/schema/{ds}/predicates", &["GET"]),
    ("/$/schema/{ds}/shapes", &["GET"]),
    ("/$/schema/{ds}/constraints", &["GET"]),
    ("/$/compact/{ds}", &["POST"]),
    ("/$/backup/{ds}", &["POST"]),
    ("/$/reason/{ds}", &["GET", "POST", "DELETE"]),
    ("/$/reason/{ds}/auto", &["PUT", "DELETE"]),
    ("/$/reason/{ds}/diagnostics", &["GET"]),
    ("/$/tasks", &["GET"]),
    // DELETE (cancel) checks admin on the task's dataset in the handler
    ("/$/tasks/{id}", &["GET", "DELETE"]),
    ("/$/prefixes/{ds}", &["GET"]),
    ("/$/cache/clear/{ds}", &["POST"]),
    ("/$/text/{ds}", &["GET", "PUT", "DELETE"]),
    ("/$/text/{ds}/rebuild", &["POST"]),
    ("/$/geo/{ds}", &["GET", "PUT", "DELETE"]),
    ("/$/geo/{ds}/rebuild", &["POST"]),
    ("/$/geo/convert", &["POST"]),
    ("/$/queries/{ds}", &["GET"]),
    ("/$/queries/{ds}/{name}", &["GET", "PUT", "DELETE"]),
    ("/$/queries/{ds}/{name}/versions", &["GET"]),
    ("/{ds}/queries/{name}", &["GET", "POST"]),
    ("/$/commits/{ds}", &["GET"]),
    ("/$/commits/{ds}/{reference}", &["GET"]),
    ("/$/vector/{ds}", &["GET"]),
    ("/$/vector/{ds}/{name}", &["GET", "PUT", "DELETE"]),
    ("/$/vector/{ds}/{name}/rebuild", &["POST"]),
    ("/$/vector/{ds}/{name}/recall", &["POST"]),
    ("/$/snapshots/{ds}", &["GET", "POST"]),
    ("/$/snapshots/{ds}/{name}", &["GET", "DELETE"]),
    ("/$/history/{ds}", &["GET", "PUT"]),
    ("/$/validation/{ds}", &["GET", "PUT", "DELETE"]),
    ("/$/rdfs/{ds}", &["GET", "PUT", "DELETE"]),
    ("/$/quota/{ds}", &["GET", "PUT", "DELETE"]),
    ("/$/compaction/{ds}", &["GET", "PUT", "DELETE"]),
    // the formatter and the linter (feature `fmt`); `serve --format-endpoint` is checked
    // by the handlers
    ("/$/format", &["POST"]),
    ("/$/lint", &["POST"]),
    // MCP (`serve --mcp`): every message is checked against the caller's datasets
    ("/$/mcp", &["*"]),
    // backup repositories (feature `backup`)
    ("/$/repositories", &["GET", "POST"]),
    ("/$/repositories/{repo}", &["GET", "PUT", "DELETE"]),
    ("/$/repositories/{repo}/test", &["POST"]),
    ("/$/repositories/{repo}/verify", &["POST"]),
    ("/$/repositories/{repo}/backups", &["GET"]),
    ("/$/repositories/{repo}/gc", &["POST"]),
    ("/$/repositories/{repo}/locks", &["GET"]),
    ("/$/repositories/{repo}/locks/{id}", &["DELETE"]),
    ("/$/backups/{ds}", &["GET", "POST"]),
    ("/$/backups/{ds}/{repo}/{backup}", &["GET", "DELETE"]),
    ("/$/backups/{ds}/{repo}/{backup}/restore", &["POST"]),
    ("/$/backups/{ds}/{repo}/{backup}/verify", &["POST"]),
    ("/$/backup-policies", &["GET", "POST"]),
    ("/$/backup-policies/preview", &["POST"]),
    ("/$/backup-policies/{policy}", &["GET", "PUT", "DELETE"]),
    ("/$/backup-policies/{policy}/run", &["POST"]),
    ("/$/backup-policies/{policy}/retention", &["POST"]),
    ("/$/backup-policies/{policy}/runs", &["GET"]),
    ("/$/auth/config", &["GET"]),
    ("/$/auth/login", &["POST"]),
    ("/$/auth/logout", &["POST"]),
    ("/$/auth/oidc/login", &["GET"]),
    ("/$/auth/oidc/callback", &["GET"]),
    ("/$/auth/oidc/backchannel-logout", &["POST"]),
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
    ("/{ds}/text", &["GET", "POST"]),
    ("/{ds}/diff", &["GET"]),
    ("/{ds}/changes", &["GET"]),
    ("/{ds}/shacl", &["POST"]),
    ("/{ds}/shex", &["POST"]),
    ("/{ds}/geo", &["GET"]),
    ("/{ds}/prefixes", &["*"]),
    // Graph Store direct naming (`serve --gsp-direct-naming`)
    ("/{ds}/{*graph}", &["*"]),
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
        | "/$/auth/oidc/backchannel-logout"
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
        // filtered by the handlers: datasets the caller may read (stats), administers
        // (backup files); the validators read no dataset
        "/$/stats"
        | "/$/backups-list"
        | "/$/validate/query"
        | "/$/validate/update"
        | "/$/validate/iri"
        | "/$/validate/data"
        | "/$/validate/langtag" => Caller,
        // reads no dataset; `--format-endpoint authenticated|off` is the handler's
        "/$/format" | "/$/lint" => Caller,
        // each tool call reads or writes the datasets its caller may (`mcp::http`)
        "/$/mcp" => Caller,
        // a pure computation over the request's literals
        "/$/geo/convert" => Caller,
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
        | "/$/schema/{ds}/shapes"
        | "/$/schema/{ds}/constraints"
        | "/$/reason/{ds}/diagnostics"
        | "/$/prefixes/{ds}"
        | "/$/commits/{ds}"
        | "/$/commits/{ds}/{reference}"
        | "/$/queries/{ds}"
        | "/$/queries/{ds}/{name}/versions"
        | "/{ds}/queries/{name}"
        | "/$/vector/{ds}"
        | "/$/vector/{ds}/{name}/recall"
        | "/{ds}/sparql"
        | "/{ds}/query"
        | "/{ds}/explain"
        | "/{ds}/text"
        | "/{ds}/diff"
        | "/{ds}/changes"
        | "/{ds}/get"
        | "/{ds}/shacl"
        | "/{ds}/shex"
        | "/{ds}/geo" => Dataset(Read),
        "/$/reason/{ds}"
        | "/$/text/{ds}"
        | "/$/geo/{ds}"
        | "/$/vector/{ds}/{name}"
        | "/$/queries/{ds}/{name}"
        | "/$/snapshots/{ds}"
        | "/$/snapshots/{ds}/{name}"
        | "/$/history/{ds}"
        | "/$/validation/{ds}"
        | "/$/rdfs/{ds}"
        | "/$/quota/{ds}"
        | "/$/compaction/{ds}"
        | "/{ds}/prefixes"
            if get =>
        {
            Dataset(Read)
        }
        // prefixes are dataset content; snapshots and history retention pin storage
        "/{ds}/prefixes" => Dataset(Write),
        // a storage quota is the operator's limit on a dataset, not its admins'
        "/$/quota/{ds}" => Server(ServerPerm::ServerAdmin),
        "/$/reason/{ds}"
        | "/$/reason/{ds}/auto"
        | "/$/text/{ds}"
        | "/$/text/{ds}/rebuild"
        | "/$/geo/{ds}"
        | "/$/geo/{ds}/rebuild"
        | "/$/vector/{ds}/{name}"
        | "/$/vector/{ds}/{name}/rebuild"
        | "/$/datasets/{ds}/clone"
        | "/$/compact/{ds}"
        | "/$/compaction/{ds}"
        | "/$/backup/{ds}"
        | "/$/cache/clear/{ds}"
        | "/$/snapshots/{ds}"
        | "/$/snapshots/{ds}/{name}"
        | "/$/history/{ds}"
        | "/$/queries/{ds}/{name}"
        | "/$/validation/{ds}"
        | "/$/rdfs/{ds}" => Dataset(Admin),
        // backups: the listing is filtered by the handler (names and types for dataset
        // admins); a backup's handlers also check that it belongs to `{ds}`, and a
        // restore needs admin on its target
        "/$/repositories" if get => Caller,
        "/$/repositories"
        | "/$/repositories/{repo}"
        | "/$/repositories/{repo}/test"
        | "/$/repositories/{repo}/verify"
        | "/$/repositories/{repo}/backups"
        | "/$/repositories/{repo}/gc"
        | "/$/repositories/{repo}/locks"
        | "/$/repositories/{repo}/locks/{id}"
        | "/$/backup-policies"
        | "/$/backup-policies/preview"
        | "/$/backup-policies/{policy}"
        | "/$/backup-policies/{policy}/run"
        | "/$/backup-policies/{policy}/retention"
        | "/$/backup-policies/{policy}/runs" => Server(ServerPerm::ServerAdmin),
        "/$/backups/{ds}" | "/$/backups/{ds}/{repo}/{backup}" if get => Dataset(Read),
        "/$/backups/{ds}"
        | "/$/backups/{ds}/{repo}/{backup}"
        | "/$/backups/{ds}/{repo}/{backup}/restore"
        | "/$/backups/{ds}/{repo}/{backup}/verify" => Dataset(Admin),
        "/{ds}/update" | "/{ds}/upload" => Dataset(Write),
        "/{ds}/data" | "/{ds}/{*graph}" if get => Dataset(Read),
        "/{ds}/data" | "/{ds}/{*graph}" => Dataset(Write),
        "/{ds}" => {
            let ct = media_type(headers);
            if has_param(uri, "update") || ct == "application/sparql-update" {
                Dataset(Write)
            } else if has_param(uri, "query")
                || ct == "application/sparql-query"
                || safe(method)
                // the body may hold `update=` or nothing usable: `dataset_root` re-checks
                // write for anything but a query
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

/// The endpoint of a request that needs a level on a dataset, for grants limited to some
/// endpoints; `None` for routes that need `admin`, which only unlimited grants give.
pub fn endpoint(route: &str, method: &Method, uri: &Uri, headers: &HeaderMap) -> Option<Endpoint> {
    let get = matches!(*method, Method::GET | Method::HEAD);
    Some(match route {
        "/{ds}/sparql"
        | "/{ds}/query"
        | "/{ds}/explain"
        | "/{ds}/text"
        | "/{ds}/geo"
        | "/{ds}/queries/{name}" => Endpoint::Query,
        "/{ds}/update" => Endpoint::Update,
        "/{ds}/get" => Endpoint::GspR,
        "/{ds}/data" | "/{ds}/{*graph}" if get => Endpoint::GspR,
        "/{ds}/data" | "/{ds}/{*graph}" => Endpoint::GspRw,
        "/{ds}/upload" => Endpoint::Upload,
        "/{ds}/shacl" => Endpoint::Shacl,
        "/{ds}/shex" => Endpoint::Shex,
        "/{ds}/diff" | "/{ds}/changes" => Endpoint::Diff,
        "/{ds}" => {
            let ct = media_type(headers);
            if has_param(uri, "update") || ct == "application/sparql-update" {
                Endpoint::Update
            } else if has_param(uri, "query")
                || ct == "application/sparql-query"
                // the body may hold `update=`: `dataset_root` re-checks
                || (*method == Method::POST && ct == "application/x-www-form-urlencoded")
            {
                Endpoint::Query
            } else if get || *method == Method::OPTIONS {
                Endpoint::GspR
            } else {
                Endpoint::GspRw
            }
        }
        r if r.contains("{ds}") => match need(route, method, uri, headers) {
            Some(Need::Dataset(Level::Read | Level::Write)) => Endpoint::Info,
            _ => return None,
        },
        _ => return None,
    })
}

/// Routes that read or change the whole dataset at once (statistics, index status,
/// reasoning diagnostics, backups, validation, prefix changes), which a caller limited to
/// some graphs may not use.
pub fn whole_dataset(route: &str, method: &Method) -> bool {
    let get = matches!(*method, Method::GET | Method::HEAD);
    match route {
        "/$/stats/{ds}"
        | "/$/reason/{ds}/diagnostics"
        | "/$/vector/{ds}"
        | "/$/vector/{ds}/{name}/recall"
        | "/{ds}/shacl"
        | "/{ds}/shex" => true,
        "/$/reason/{ds}"
        | "/$/text/{ds}"
        | "/$/geo/{ds}"
        | "/$/vector/{ds}/{name}"
        | "/$/backups/{ds}"
        | "/$/backups/{ds}/{repo}/{backup}"
        | "/$/history/{ds}"
        | "/$/rdfs/{ds}"
        | "/$/quota/{ds}"
        | "/$/compaction/{ds}" => get,
        "/{ds}/prefixes" => !get,
        _ => false,
    }
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

/// OWASP's origin verification with standard headers: an `Origin` that is neither the
/// server's own nor an allowed CORS origin, or else a cross-site `Sec-Fetch-Site` (an
/// allowed origin's pages are cross-site too).
fn cross_origin(h: &HeaderMap, allowed: impl Fn(&str) -> bool, public_url: Option<&str>) -> bool {
    let origin = h.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    if origin.is_some_and(&allowed) {
        return false;
    }
    if h.get("sec-fetch-site").is_some_and(|v| v == "cross-site") {
        return true;
    }
    origin.is_some_and(|o| !own_origin(o, h, public_url))
}

/// Whether a request of `method` that needs `need` passes the origin gate only when it
/// comes from the server's own pages (or an allowed origin): unsafe methods, and
/// anything needing `write`, `admin` or `server-admin`.
fn origin_gated(method: &Method, need: Need) -> bool {
    !safe(method)
        || matches!(
            need,
            Need::Dataset(Level::Write | Level::Admin) | Need::Server(ServerPerm::ServerAdmin)
        )
}

/// The `Host` of a request (`:authority` over HTTP/2).
fn host_of(req: &Request) -> Option<&str> {
    match req.headers().get(header::HOST) {
        // not text: no name it is known by
        Some(v) => Some(v.to_str().unwrap_or("")),
        None => req.uri().authority().map(|a| a.as_str()),
    }
}

/// With trusted proxy headers accepted from a local peer (a loopback address or the Unix
/// socket), a request that carries them must name a `Host` the server is known by: an IP
/// address, `localhost`, `--host`, a `--public-host` name or the host of
/// `server.public_url`. Otherwise a web page that rebinds its DNS name to the server (or
/// to the proxy in front of it) could send its own `Remote-User`; `421`.
#[cfg(feature = "auth")]
fn local_proxy_gate(
    st: &AppState,
    policy: &super::policy::Policy,
    peer: Option<&super::Peer>,
    req: &Request,
) -> Option<Response> {
    let px = policy.proxy.as_ref()?;
    let local = match peer? {
        super::Peer::Unix => true,
        super::Peer::Tcp(a) => a.ip().to_canonical().is_loopback(),
    };
    if !local || !px.has_headers(req.headers()) || !px.trusted.trusts(peer) {
        return None;
    }
    let host = host_of(req)?;
    let public = policy
        .public_url
        .as_deref()
        .and_then(|u| reqwest::Url::parse(u).ok())
        .and_then(|u| u.host_str().map(str::to_string));
    let known = st.hosts.allows(host)
        || public.is_some_and(|p| {
            crate::exposure::Hosts::new("unix", &[p]).is_ok_and(|h| h.allows(host))
        });
    (!known).then(|| {
        json_error(
            StatusCode::MISDIRECTED_REQUEST,
            &format!(
                "this server does not take proxy identity headers for host '{host}' (it \
                 answers IP addresses, localhost, --host and --public-host names, and the \
                 host of server.public_url)"
            ),
        )
    })
}

/// Without auth every caller is the local principal, so the only thing between a web
/// page and the server's data is the browser: refuse a `Host` the server is not known
/// by (DNS rebinding, `421`), and the requests the origin gate refuses with auth
/// (`403`). Clients that are not browsers (the CLI, curl, other servers) send no
/// `Origin` or `Sec-Fetch-Site` and pass.
fn open_gate(st: &AppState, req: &Request) -> Option<Response> {
    if let Some(host) = host_of(req)
        && !st.hosts.allows(host)
    {
        let msg = format!(
            "this server does not answer for host '{host}' (without --auth-config it \
             answers IP addresses, localhost, --host and --public-host names)"
        );
        return Some(json_error(StatusCode::MISDIRECTED_REQUEST, &msg));
    }
    let route = req.extensions().get::<MatchedPath>()?.as_str();
    let need = need(route, req.method(), req.uri(), req.headers())
        .unwrap_or(Need::Server(ServerPerm::ServerAdmin));
    if origin_gated(req.method(), need)
        && cross_origin(req.headers(), |o| super::api::cors_allowed(st, o), None)
    {
        return Some(json_error(
            StatusCode::FORBIDDEN,
            "cross-origin request refused",
        ));
    }
    None
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
    let body = json!({ "error": msg });
    let mut r = (status, axum::Json(body.clone())).into_response();
    // the error gets the request id like every other error body
    r.extensions_mut().insert(crate::http::ErrorJson(body));
    r
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

/// For a handler that learns its operation from the body (`POST /{ds}` with a form):
/// the answer the middleware gives a caller below `lvl` on `ds`, or `None` when the
/// caller has it. The same responses as the route table's: 401 to anonymous, the
/// handlers' 404 when the dataset is hidden or missing, else 403.
pub fn dataset_denial(
    st: &AppState,
    p: &Principal,
    headers: &HeaderMap,
    ds: &str,
    lvl: Level,
    e: Endpoint,
) -> Option<Response> {
    let have = p.level(ds);
    if p.level_at(ds, e).is_some_and(|h| h >= lvl) {
        return None;
    }
    if p.is_anonymous() {
        return Some(authentication_required(st, p, headers));
    }
    if have.is_none() || st.get(ds).is_none() {
        let r = json_error(StatusCode::NOT_FOUND, &format!("no such dataset: /{ds}"));
        return Some(with_report(r, report_of(p, Some(Denied::Hidden))));
    }
    Some(forbidden(p, &denial_message(ds, lvl, e, have)))
}

/// The 403 of a caller below `lvl` on `ds` through endpoint `e`: the endpoint is named
/// when another endpoint would have given the level.
fn denial_message(ds: &str, lvl: Level, e: Endpoint, have: Option<Level>) -> String {
    if have.is_some_and(|h| h >= lvl) {
        format!("the {} endpoint of /{ds} is not allowed", e.as_str())
    } else {
        format!("{} access to /{ds} required", lvl.as_str())
    }
}

/// The `401` with the server's challenges, for an anonymous caller that must sign in
/// (the MCP endpoint, when anonymous callers can read no dataset).
pub fn authentication_required(st: &AppState, p: &Principal, headers: &HeaderMap) -> Response {
    #[cfg(feature = "auth")]
    let (realm, basic) = match &st.auth {
        Some(a) => {
            let policy = a.policy();
            (policy.realm.clone(), policy.has_users())
        }
        None => ("sparkles".to_string(), false),
    };
    #[cfg(not(feature = "auth"))]
    let (realm, basic) = {
        let _ = st;
        ("sparkles".to_string(), false)
    };
    let r = unauthorized(&realm, headers, "authentication required", None, basic);
    with_report(r, report_of(p, Some(Denied::Unauthenticated)))
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
        let id = req
            .headers()
            .get(&crate::obs::X_REQUEST_ID)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let mut resp = enforce(&st, &auth, req, next).await;
        crate::http::add_request_id(&mut resp, id);
        return resp;
    }
    if let Some(mut r) = open_gate(&st, &req) {
        let id = req
            .headers()
            .get(&crate::obs::X_REQUEST_ID)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        crate::http::add_request_id(&mut r, id);
        return r;
    }
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
    // identity headers from a local proxy (loopback or the Unix socket) can also come
    // from a web page that rebinds its DNS name to the server: they need a known `Host`
    if let Some(r) = local_proxy_gate(st, &policy, peer.as_ref(), &req) {
        count(Denied::Forbidden);
        return with_report(
            r,
            AuthReport {
                principal: None,
                scheme: Some("proxy"),
                denied: Some(Denied::Forbidden),
                error: None,
            },
        );
    }
    let admission = req
        .extensions()
        .get::<crate::ratelimit::Admission>()
        .cloned();
    let client = req
        .extensions()
        .get::<crate::ratelimit::ClientAddr>()
        .map(|c| c.0.network());
    let authed = match auth
        .authenticate(
            req.headers(),
            peer.as_ref(),
            admission.as_ref(),
            client.as_ref(),
        )
        .await
    {
        Ok(a) => a,
        Err(mut e) => {
            // credentials that were checked and failed (not a busy check, this limit's
            // refusal, or a proxy identity that is not admitted)
            let failed = matches!(
                e.failure,
                Failure::Malformed | Failure::Invalid | Failure::Expired
            );
            // an address without failures left has its unknown tokens refused like its
            // password checks
            let exhausted = failed && admission.as_ref().is_some_and(|a| a.exhausted());
            if exhausted {
                e.failure = Failure::Limited;
            }
            auth.count_failure(e);
            span.record("principal", "-");
            span.record("auth", e.scheme);
            let mut r = match e.failure {
                // the address spent its failures: the pre-authentication limit's 429
                Failure::Limited => match &admission {
                    Some(a) => a.refusal(),
                    None => json_error(StatusCode::TOO_MANY_REQUESTS, "too many failures"),
                },
                Failure::Busy => {
                    let mut r = json_error(StatusCode::SERVICE_UNAVAILABLE, "authentication busy");
                    r.headers_mut()
                        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
                    r
                }
                // the identity provider's keys could not be fetched to check a JWT
                Failure::Idp => {
                    let mut r = json_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "identity provider unavailable",
                    );
                    r.headers_mut()
                        .insert(header::RETRY_AFTER, HeaderValue::from_static("5"));
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
            if failed && !exhausted {
                r.extensions_mut().insert(crate::ratelimit::AuthFailed);
            }
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
            // an invalid session cookie is a failed authentication, though not refused
            r.extensions_mut().insert(crate::ratelimit::AuthFailed);
        }
        r
    };
    // a refused origin or CSRF token on an auth route (a login, a token, a device
    // approval) is a failed authentication too
    let auth_route = route.starts_with("/$/auth/");
    let csrf_failed = |mut r: Response| {
        if auth_route {
            r.extensions_mut().insert(crate::ratelimit::AuthFailed);
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
    if origin_gated(&method, need)
        && cross_origin(
            req.headers(),
            |o| super::api::cors_allowed(st, o),
            policy.public_url.as_deref(),
        )
    {
        let r = json_error(StatusCode::FORBIDDEN, "cross-origin request refused");
        return finish(csrf_failed(deny(Denied::CrossOrigin, r, &p)));
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
            return finish(csrf_failed(deny(Denied::Csrf, r, &p)));
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
            // through the route's endpoint, for grants limited to some endpoints
            let e = endpoint(&route, &method, req.uri(), req.headers());
            let through = match e {
                Some(e) => p.level_at(&ds, e),
                None => have,
            };
            if through.is_none_or(|h| h < lvl) {
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
                let msg = match e {
                    Some(e) => denial_message(&ds, lvl, e, have),
                    None => format!("{} access to /{ds} required", lvl.as_str()),
                };
                return finish(forbidden(&p, &msg));
            }
            // routes that report on, or change, every graph at once refuse a caller whose
            // grants cover only some graphs
            if let Some(e) = e
                && whole_dataset(&route, &method)
                && p.view(&ds, e).is_some()
            {
                count(Denied::Forbidden);
                let msg = format!(
                    "{} covers every graph of /{ds}, and your access is limited to some graphs",
                    route.replace("{ds}", &ds)
                );
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
    fn validation_routes_read_the_dataset() {
        for route in ["/{ds}/shacl", "/{ds}/shex"] {
            let n = need(route, &Method::POST, &"/ds/x".parse().unwrap(), &h(&[]));
            assert_eq!(n, Some(Need::Dataset(Level::Read)), "{route}");
        }
    }

    #[test]
    fn a_quota_is_read_by_readers_and_set_by_server_admins() {
        let n = |m: Method| need("/$/quota/{ds}", &m, &"/x".parse().unwrap(), &h(&[]));
        assert_eq!(n(Method::GET), Some(Need::Dataset(Level::Read)));
        for m in [Method::PUT, Method::DELETE] {
            assert_eq!(n(m), Some(Need::Server(ServerPerm::ServerAdmin)));
        }
    }

    #[test]
    fn backup_route_needs() {
        let n = |m: Method, route: &str| need(route, &m, &"/x".parse().unwrap(), &h(&[]));
        let admin = Some(Need::Server(ServerPerm::ServerAdmin));
        assert_eq!(n(Method::GET, "/$/repositories"), Some(Need::Caller));
        assert_eq!(n(Method::POST, "/$/repositories"), admin);
        for route in [
            "/$/repositories/{repo}",
            "/$/repositories/{repo}/backups",
            "/$/repositories/{repo}/locks",
            "/$/backup-policies",
            "/$/backup-policies/{policy}/runs",
        ] {
            assert_eq!(n(Method::GET, route), admin, "{route}");
        }
        for route in [
            "/$/repositories/{repo}/test",
            "/$/repositories/{repo}/verify",
            "/$/repositories/{repo}/gc",
            "/$/backup-policies/preview",
            "/$/backup-policies/{policy}/run",
            "/$/backup-policies/{policy}/retention",
        ] {
            assert_eq!(n(Method::POST, route), admin, "{route}");
        }
        assert_eq!(
            n(Method::DELETE, "/$/repositories/{repo}/locks/{id}"),
            admin
        );
        let read = Some(Need::Dataset(Level::Read));
        let adm = Some(Need::Dataset(Level::Admin));
        assert_eq!(n(Method::GET, "/$/backups/{ds}"), read);
        assert_eq!(n(Method::POST, "/$/backups/{ds}"), adm);
        assert_eq!(n(Method::GET, "/$/backups/{ds}/{repo}/{backup}"), read);
        assert_eq!(n(Method::DELETE, "/$/backups/{ds}/{repo}/{backup}"), adm);
        assert_eq!(
            n(Method::POST, "/$/backups/{ds}/{repo}/{backup}/restore"),
            adm
        );
        assert_eq!(
            n(Method::POST, "/$/backups/{ds}/{repo}/{backup}/verify"),
            adm
        );
        // the dataset of a backup route is its first parameter
        let uri: Uri = "/$/backups/wiki/local/b1/restore".parse().unwrap();
        assert_eq!(
            ds_of("/$/backups/{ds}/{repo}/{backup}/restore", &uri).as_deref(),
            Some("wiki")
        );
        // cancelling is checked by the handler
        assert_eq!(n(Method::DELETE, "/$/tasks/{id}"), Some(Need::Caller));
        // formatting and linting read no dataset
        assert_eq!(n(Method::POST, "/$/format"), Some(Need::Caller));
        assert_eq!(n(Method::POST, "/$/lint"), Some(Need::Caller));
    }

    #[test]
    fn geo_route_needs() {
        let n = |m: Method, route: &str| need(route, &m, &"/x".parse().unwrap(), &h(&[]));
        let read = Some(Need::Dataset(Level::Read));
        let adm = Some(Need::Dataset(Level::Admin));
        assert_eq!(n(Method::GET, "/$/geo/{ds}"), read);
        assert_eq!(n(Method::PUT, "/$/geo/{ds}"), adm);
        assert_eq!(n(Method::DELETE, "/$/geo/{ds}"), adm);
        assert_eq!(n(Method::POST, "/$/geo/{ds}/rebuild"), adm);
        assert_eq!(n(Method::GET, "/{ds}/geo"), read);
        assert_eq!(n(Method::POST, "/$/geo/convert"), Some(Need::Caller));
        let uri: Uri = "/$/geo/places/rebuild".parse().unwrap();
        assert_eq!(
            ds_of("/$/geo/{ds}/rebuild", &uri).as_deref(),
            Some("places")
        );
    }

    #[test]
    fn origins() {
        let allowed = |o: &str| o == "https://yasgui.example";
        let x = |pairs: &[(&str, &str)]| cross_origin(&h(pairs), allowed, None);
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
        // an allowed origin's pages are cross-site
        assert!(!x(&[
            ("host", "a:3030"),
            ("origin", "https://yasgui.example"),
            ("sec-fetch-site", "cross-site")
        ]));
        assert!(x(&[
            ("host", "a:3030"),
            ("origin", "http://a:3030"),
            ("sec-fetch-site", "cross-site")
        ]));
        assert!(!x(&[("host", "a:3030"), ("sec-fetch-site", "same-origin")]));
        // with a public URL, Host does not matter
        let pu = Some("https://sparql.example.org");
        assert!(!cross_origin(
            &h(&[("host", "evil"), ("origin", "https://sparql.example.org")]),
            allowed,
            pu
        ));
        assert!(cross_origin(
            &h(&[("host", "a:3030"), ("origin", "http://a:3030")]),
            allowed,
            pu
        ));
    }
}
