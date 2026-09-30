//! What the rest of the server uses from the auth layer: CORS by policy, the query
//! restrictions of a principal, `/$/whoami` and the `auth` member of `/$/server`.

use super::{Level, Principal, ServerPerm};
use crate::state::AppState;
use axum::extract::{Extension, State};
use axum::http::{HeaderName, Method, header};
use serde_json::{Map, Value as J, json};
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, CorsLayer};

/// CORS: today's permissive layer without auth; with auth, only the configured origins,
/// never with credentials (the policy is read per request, so reloads apply).
pub fn cors_layer(st: &AppState, expose: Vec<HeaderName>) -> CorsLayer {
    #[cfg(feature = "auth")]
    if let Some(auth) = st.auth.clone() {
        let mut expose = expose;
        expose.push(header::WWW_AUTHENTICATE);
        return CorsLayer::new()
            .allow_origin(AllowOrigin::predicate(move |origin, _| {
                let o = origin.to_str().unwrap_or("");
                auth.policy()
                    .cors_origins
                    .iter()
                    .any(|a| a.eq_ignore_ascii_case(o))
            }))
            .allow_headers([
                header::AUTHORIZATION,
                header::CONTENT_TYPE,
                header::ACCEPT,
                crate::obs::X_REQUEST_ID.clone(),
            ])
            .allow_methods([
                Method::GET,
                Method::HEAD,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .expose_headers(expose);
    }
    let _ = st;
    CorsLayer::very_permissive().expose_headers(expose)
}

/// Outbound requests and local files are server capabilities: SERVICE and
/// `LOAD <http…>` need `federate`, `LOAD <file:…>` needs `server-admin`.
pub fn restrict(opts: &mut sparkles::sparql::QueryOptions, p: &Principal) {
    opts.forbid_service = !p.has(ServerPerm::Federate);
    opts.forbid_remote_load = !p.has(ServerPerm::Federate);
    opts.forbid_file_load = !p.has(ServerPerm::ServerAdmin);
}

/// The `auth` member of `/$/server`.
pub fn server_json(st: &AppState) -> J {
    match &st.auth {
        #[cfg(feature = "auth")]
        Some(_) => json!({ "enabled": true }),
        _ => json!({ "enabled": false }),
    }
}

/// `serve --auth-config`: load and validate the configuration (an error stops the
/// server before it binds) and log its warnings.
pub fn load(
    path: Option<&std::path::Path>,
    data_dir: &std::path::Path,
    host: &str,
) -> anyhow::Result<Option<Arc<super::Auth>>> {
    let Some(path) = path else {
        return Ok(None);
    };
    #[cfg(feature = "auth")]
    {
        let (auth, warnings) = super::Auth::open(path, data_dir)?;
        for w in warnings {
            tracing::warn!("auth configuration: {w}");
        }
        let loopback = matches!(host, "127.0.0.1" | "::1" | "localhost" | "[::1]" | "unix");
        if !loopback {
            tracing::warn!(
                "credentials are accepted over plain HTTP on {host}; terminate TLS in front of the server"
            );
            if auth.policy().proxy.is_some() {
                tracing::warn!(
                    "trusted-header auth is enabled and the server listens on {host}: any host in proxy.trusted can impersonate any user; make sure only the proxy can reach this port"
                );
            }
        }
        tracing::info!("authentication enabled ({})", path.display());
        Ok(Some(Arc::new(auth)))
    }
    #[cfg(not(feature = "auth"))]
    {
        let _ = (host, data_dir);
        anyhow::bail!(
            "--auth-config {}: built without authentication (cargo feature \"auth\")",
            path.display()
        )
    }
}

/// The `/$/whoami` and `/$/auth/*` routes (without auth, `/$/auth/config` says so and
/// the other `/$/auth/*` routes do not exist).
pub fn routes() -> axum::Router<Arc<AppState>> {
    use axum::routing::get;
    let r = axum::Router::new().route("/$/whoami", get(whoami));
    #[cfg(feature = "auth")]
    return r.merge(super::handlers::routes());
    #[cfg(not(feature = "auth"))]
    r.route(
        "/$/auth/config",
        get(|| async { axum::Json(json!({ "enabled": false })) }),
    )
}

/// Write state kept in memory (token `lastUsed` times) at shutdown, and prune
/// expired sessions hourly while running.
pub fn flush(st: &AppState) {
    #[cfg(feature = "auth")]
    if let Some(a) = &st.auth
        && let Err(e) = a.tokens.flush()
    {
        tracing::warn!("cannot write the token store: {e:#}");
    }
    let _ = st;
}

/// Re-read the auth configuration on SIGHUP (Unix; the policy is swapped atomically,
/// and a bad file keeps the old one).
pub fn spawn_reload_on_sighup(st: &Arc<AppState>) {
    #[cfg(all(unix, feature = "auth"))]
    if let Some(auth) = st.auth.clone() {
        tokio::spawn(async move {
            use tokio::signal::unix::{SignalKind, signal};
            let Ok(mut hup) = signal(SignalKind::hangup()) else {
                tracing::warn!("cannot listen for SIGHUP: auth reload disabled");
                return;
            };
            let prune_auth = auth.clone();
            tokio::spawn(async move {
                let mut t = tokio::time::interval(std::time::Duration::from_secs(3600));
                loop {
                    t.tick().await;
                    let now = prune_auth.now();
                    if let Err(e) = prune_auth.sessions.prune(now) {
                        tracing::warn!("cannot prune sessions: {e:#}");
                    }
                }
            });
            // discovery at startup: a provider that is down only delays logins
            if let Some(c) = auth.oidc.client() {
                tokio::spawn(async move {
                    if let Err(e) = c.metadata().await {
                        tracing::warn!("OIDC discovery failed (logins retry it): {e:#}");
                    }
                });
            }
            while hup.recv().await.is_some() {
                match auth.reload() {
                    Ok(warnings) => {
                        tracing::info!("auth configuration reloaded");
                        for w in warnings {
                            tracing::warn!("auth configuration: {w}");
                        }
                    }
                    Err(e) => tracing::error!("auth configuration not reloaded: {e:#}"),
                }
            }
        });
    }
    let _ = st;
}

/// The auth metric families of the Prometheus exposition (nothing without auth).
pub fn render_metrics(st: &AppState, o: &mut String) {
    #[cfg(feature = "auth")]
    if let Some(a) = &st.auth {
        a.render_metrics(o);
    }
    let _ = (st, o);
}

/// `GET /$/whoami`: the caller and its effective permissions on existing datasets.
pub async fn whoami(
    State(st): State<Arc<AppState>>,
    Extension(p): Extension<Principal>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let mut datasets = Map::new();
    for name in st.datasets.read().keys() {
        let lvl = if p.is_local() {
            Some(Level::Admin)
        } else {
            p.level(name)
        };
        if let Some(l) = lvl {
            datasets.insert(name.clone(), l.as_str().into());
        }
    }
    let mut principal = json!({ "kind": p.kind.as_str() });
    if !p.is_local() && !p.is_anonymous() {
        principal["name"] = p.name.to_string().into();
    }
    let server: Vec<&str> = p.server_perms().iter().map(|s| s.as_str()).collect();
    #[allow(unused_mut)]
    let mut doc = json!({
        "authEnabled": st.auth.is_some(),
        "principal": principal,
        "method": p.scheme.as_str(),
        "server": server,
        "datasets": datasets,
        "canMintTokens": false,
        "logout": false,
    });
    #[cfg(feature = "auth")]
    if let Some(a) = &st.auth {
        super::handlers::whoami_details(a, &p, &mut doc);
    }
    let mut r = axum::Json(doc).into_response();
    r.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    r
}
