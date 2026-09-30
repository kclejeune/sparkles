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
        Some(_) => json!({ "enabled": true, "schemes": ["Bearer", "Basic"] }),
        _ => json!({ "enabled": false }),
    }
}

/// `serve --auth-config`: load and validate the configuration (an error stops the
/// server before it binds) and log its warnings.
pub fn load(
    path: Option<&std::path::Path>,
    host: &str,
) -> anyhow::Result<Option<Arc<super::Auth>>> {
    let Some(path) = path else {
        return Ok(None);
    };
    #[cfg(feature = "auth")]
    {
        let (auth, warnings) = super::Auth::load(path)?;
        for w in warnings {
            tracing::warn!("auth configuration: {w}");
        }
        let loopback = matches!(host, "127.0.0.1" | "::1" | "localhost" | "[::1]");
        if !loopback {
            tracing::warn!(
                "credentials are accepted over plain HTTP on {host}; terminate TLS in front of the server"
            );
        }
        tracing::info!("authentication enabled ({})", path.display());
        Ok(Some(Arc::new(auth)))
    }
    #[cfg(not(feature = "auth"))]
    {
        let _ = host;
        anyhow::bail!(
            "--auth-config {}: built without authentication (cargo feature \"auth\")",
            path.display()
        )
    }
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
) -> axum::Json<J> {
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
    axum::Json(json!({
        "authEnabled": st.auth.is_some(),
        "principal": principal,
        "server": server,
        "datasets": datasets,
    }))
}
