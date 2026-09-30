//! Authentication and dataset-level authorization.
//!
//! Without an auth configuration (`serve --auth-config`) every request runs as the
//! [`Principal::local`] principal, which may do everything: the server behaves as if
//! this module did not exist. With one, [`middleware`] authenticates each request,
//! computes the permission its route needs from one table ([`need`]), and denies by
//! default.
//!
//! The permission model: per-dataset levels `read` < `write` < `admin`, granted by
//! dataset name or `*` pattern, plus the server permissions `metrics`, `federate` and
//! `server-admin`. Grants are a union; there are no deny rules.

// without the feature only the local principal and the route table remain
#![cfg_attr(not(feature = "auth"), allow(dead_code, unused_imports))]

use serde::{Deserialize, Serialize};
use std::sync::Arc;

mod api;
pub mod proxy;
mod routes;

#[cfg(feature = "auth")]
pub mod cli;
#[cfg(feature = "auth")]
pub mod config;
#[cfg(feature = "auth")]
pub mod crypto;
#[cfg(feature = "auth")]
mod grants;
#[cfg(feature = "auth")]
mod handlers;
#[cfg(feature = "auth")]
pub mod oidc;
#[cfg(feature = "auth")]
mod policy;
#[cfg(feature = "auth")]
mod session;
#[cfg(feature = "auth")]
mod store;
#[cfg(feature = "auth")]
mod tokens;

pub use api::{
    PrincipalKeyer, cors_layer, flush, load, render_metrics, restrict, routes, server_json,
    spawn_reload_on_sighup, throttle,
};
pub use proxy::Peer;
pub use routes::{AuthReport, Denied, dataset_denial, forbidden, middleware};
#[cfg(test)]
pub use routes::{ROUTES, need};

#[cfg(all(test, feature = "auth"))]
pub use handlers::MAX_AUTH_BODY;
#[cfg(feature = "auth")]
pub use handlers::SESSION_COOKIE;
#[cfg(feature = "auth")]
pub use policy::Auth;
#[cfg(all(test, feature = "auth"))]
pub use policy::{hash_password, hash_password_with, new_token, token_hash};
#[cfg(all(test, feature = "auth"))]
pub use session::MAX_SESSIONS_PER_OWNER;
#[cfg(feature = "auth")]
pub use store::write_private;

/// Without the `auth` feature there is never an [`Auth`].
#[cfg(not(feature = "auth"))]
pub enum Auth {}

/// A dataset access level; the levels are ordered and each includes the ones below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Read,
    Write,
    Admin,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Read => "read",
            Level::Write => "write",
            Level::Admin => "admin",
        }
    }
}

/// A server-wide permission.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerPerm {
    /// `GET /$/metrics`, the full dataset list of `/$/ready`
    Metrics,
    /// outbound HTTP from queries and updates (`SERVICE`, `LOAD <http…>`)
    Federate,
    /// everything
    ServerAdmin,
}

impl ServerPerm {
    pub const ALL: [ServerPerm; 3] = [
        ServerPerm::Metrics,
        ServerPerm::Federate,
        ServerPerm::ServerAdmin,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ServerPerm::Metrics => "metrics",
            ServerPerm::Federate => "federate",
            ServerPerm::ServerAdmin => "server-admin",
        }
    }
}

/// `*` matches any run (possibly empty) of characters; everything else matches itself.
pub fn glob(pattern: &str, name: &str) -> bool {
    let (p, n) = (pattern.as_bytes(), name.as_bytes());
    // iterative wildcard matching with backtracking to the last `*`
    let (mut i, mut j) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while j < n.len() {
        if i < p.len() && p[i] == b'*' {
            star = Some((i, j));
            i += 1;
        } else if i < p.len() && p[i] == n[j] {
            i += 1;
            j += 1;
        } else if let Some((si, sj)) = star {
            i = si + 1;
            j = sj + 1;
            star = Some((si, sj + 1));
        } else {
            return false;
        }
    }
    p[i..].iter().all(|&c| c == b'*')
}

/// Dataset grants by pattern and server permissions; roles are already flattened in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    pub datasets: Vec<(String, Level)>,
    pub server: Vec<ServerPerm>,
}

impl Grants {
    /// The highest level any matching pattern grants (independent of whether the
    /// dataset exists); `admin` everywhere with `server-admin`.
    pub fn level(&self, ds: &str) -> Option<Level> {
        if self.server.contains(&ServerPerm::ServerAdmin) {
            return Some(Level::Admin);
        }
        self.datasets
            .iter()
            .filter(|(p, _)| glob(p, ds))
            .map(|(_, l)| *l)
            .max()
    }

    pub fn has(&self, p: ServerPerm) -> bool {
        self.server.contains(&ServerPerm::ServerAdmin) || self.server.contains(&p)
    }

    /// Add `other`'s grants.
    pub fn extend(&mut self, other: &Grants) {
        for g in &other.datasets {
            if !self.datasets.contains(g) {
                self.datasets.push(g.clone());
            }
        }
        for s in &other.server {
            if !self.server.contains(s) {
                self.server.push(*s);
            }
        }
    }
}

/// The scope of a minted token: an upper bound, intersected with its owner's (or
/// parent's) permissions at each use. Unlike [`Grants`], `server-admin` in a scope does
/// not widen its dataset levels.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scope {
    #[serde(default)]
    pub datasets: std::collections::BTreeMap<String, Level>,
    /// server permissions, or `"*"`: all of the owner's
    #[serde(default)]
    pub server: Vec<String>,
}

impl Scope {
    /// "Everything I have": `{"*": "admin"}` and `["*"]`.
    #[cfg(test)]
    pub fn all() -> Scope {
        Scope {
            datasets: [("*".to_string(), Level::Admin)].into(),
            server: vec!["*".into()],
        }
    }

    fn level(&self, ds: &str) -> Option<Level> {
        self.datasets
            .iter()
            .filter(|(p, _)| glob(p, ds))
            .map(|(_, l)| *l)
            .max()
    }

    fn has(&self, p: ServerPerm) -> bool {
        self.server
            .iter()
            .any(|s| s == "*" || s == p.as_str() || s == ServerPerm::ServerAdmin.as_str())
    }

    /// A short human summary: `wiki=read, *=admin; metrics`.
    pub fn summary(&self) -> String {
        let ds: Vec<String> = self
            .datasets
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_str()))
            .collect();
        let mut s = if ds.is_empty() {
            "no datasets".to_string()
        } else {
            ds.join(", ")
        };
        if !self.server.is_empty() {
            s.push_str("; ");
            s.push_str(&self.server.join(", "));
        }
        s
    }
}

/// Effective permissions: a principal's grants, narrowed by the scopes of the tokens
/// it acts through (a token chain adds one scope per link).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub grants: Grants,
    pub scopes: Vec<Scope>,
}

impl Access {
    pub fn of(grants: Grants) -> Access {
        Access {
            grants,
            scopes: Vec::new(),
        }
    }

    pub fn level(&self, ds: &str) -> Option<Level> {
        let mut l = self.grants.level(ds)?;
        for s in &self.scopes {
            l = l.min(s.level(ds)?);
        }
        Some(l)
    }

    pub fn has(&self, p: ServerPerm) -> bool {
        self.grants.has(p) && self.scopes.iter().all(|s| s.has(p))
    }
}

/// What kind of caller a principal is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// auth is disabled: everything is allowed
    Local,
    Anonymous,
    /// a configured user (HTTP Basic, or a UI password login)
    User,
    /// an API token (static or minted)
    Token,
    /// a UI login through the OIDC provider
    Oidc,
    /// trusted headers from a forward-auth proxy
    Proxy,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Local => "local",
            Kind::Anonymous => "anonymous",
            Kind::User => "user",
            Kind::Token => "token",
            Kind::Oidc => "oidc",
            Kind::Proxy => "proxy",
        }
    }
}

/// How the request authenticated (the access log's `auth` field, whoami's `method`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    None,
    Basic,
    Bearer,
    Session,
    Proxy,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::None => "none",
            Scheme::Basic => "basic",
            Scheme::Bearer => "bearer",
            Scheme::Session => "session",
            Scheme::Proxy => "proxy",
        }
    }
}

/// An identity that can own tokens and sessions: a configured user, or an OIDC or
/// proxy account with the groups it had when it signed in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub kind: Kind,
    pub name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(
        default,
        rename = "displayName",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,
}

impl Identity {
    /// `user:bob`, `oidc:alice@example.org`
    pub fn log_name(&self) -> String {
        format!("{}:{}", self.kind.as_str(), self.name)
    }
}

/// Details of an authenticated principal beyond its permissions.
#[derive(Clone, Debug, Default)]
pub struct PrincipalInfo {
    /// who tokens minted by this principal belong to (`None`: anonymous, static token)
    pub owner: Option<Identity>,
    /// the minted token in use (the parent of tokens it mints)
    pub token_id: Option<String>,
    /// a static token from the configuration (cannot mint)
    pub static_token: bool,
    /// Unix seconds at which the credential (token or session) expires
    pub expires: Option<i64>,
    /// the CSRF token an ambient principal (session or proxy) must send
    pub csrf: Option<String>,
    /// digest of the session id (sessions only)
    pub session: Option<[u8; 32]>,
}

/// The caller of a request, inserted into the request extensions by [`middleware`].
/// Later layers and handlers read it with `Extension<Principal>`; [`Principal::id`] is a
/// stable key (never a credential).
#[derive(Clone)]
pub struct Principal {
    pub kind: Kind,
    /// user name, token id, or OIDC / proxy account name
    pub name: Arc<str>,
    pub scheme: Scheme,
    access: Arc<Access>,
    pub info: Arc<PrincipalInfo>,
}

impl std::fmt::Debug for Principal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Principal")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("scheme", &self.scheme)
            .finish()
    }
}

impl Principal {
    /// Auth disabled: everything is allowed.
    pub fn local() -> Principal {
        static LOCAL: std::sync::LazyLock<Principal> = std::sync::LazyLock::new(|| Principal {
            kind: Kind::Local,
            name: "".into(),
            scheme: Scheme::None,
            access: Arc::new(Access::of(Grants {
                datasets: Vec::new(),
                server: vec![ServerPerm::ServerAdmin],
            })),
            info: Arc::default(),
        });
        LOCAL.clone()
    }

    pub fn new(kind: Kind, name: &str, scheme: Scheme, access: Access) -> Principal {
        Principal {
            kind,
            name: name.into(),
            scheme,
            access: Arc::new(access),
            info: Arc::default(),
        }
    }

    pub fn with_info(mut self, info: PrincipalInfo) -> Principal {
        self.info = Arc::new(info);
        self
    }

    pub fn is_local(&self) -> bool {
        self.kind == Kind::Local
    }

    pub fn is_anonymous(&self) -> bool {
        self.kind == Kind::Anonymous
    }

    /// Session and proxy principals: the browser attaches their credential by itself.
    pub fn is_ambient(&self) -> bool {
        matches!(self.scheme, Scheme::Session | Scheme::Proxy)
    }

    /// Only an interactive principal may approve CLI logins.
    pub fn is_interactive(&self) -> bool {
        self.is_ambient()
    }

    /// The level granted on dataset `ds` (whether or not it exists).
    pub fn level(&self, ds: &str) -> Option<Level> {
        self.access.level(ds)
    }

    pub fn can(&self, ds: &str, need: Level) -> bool {
        self.level(ds).is_some_and(|l| l >= need)
    }

    /// `server-admin` implies every server permission.
    pub fn has(&self, p: ServerPerm) -> bool {
        self.access.has(p)
    }

    /// A stable key for this principal (its log name, `local` without auth): what rate
    /// limiters and audit records key on.
    pub fn id(&self) -> String {
        self.log_name().unwrap_or_else(|| "local".into())
    }

    /// The key of per-client rate limits: the owner of a minted token or a session
    /// (`user:bob`), so that all the credentials of one owner share one budget; else
    /// [`Principal::id`] (a static token of the configuration is a client of its own).
    pub fn rate_key(&self) -> String {
        match &self.info.owner {
            Some(o) if !self.info.static_token => o.log_name(),
            _ => self.id(),
        }
    }

    /// `anonymous`, `user:bob`, `token:tok_…`, `oidc:…`, `proxy:…`; `None` when auth is
    /// disabled.
    pub fn log_name(&self) -> Option<String> {
        match self.kind {
            Kind::Local => None,
            Kind::Anonymous => Some("anonymous".into()),
            k => Some(format!("{}:{}", k.as_str(), self.name)),
        }
    }

    /// The `access` field of a dataset in listings: absent when auth is disabled.
    pub fn access(&self, ds: &str) -> Option<Level> {
        if self.is_local() {
            None
        } else {
            self.level(ds)
        }
    }

    /// The server permissions held, for `whoami`.
    pub fn server_perms(&self) -> Vec<ServerPerm> {
        if self.has(ServerPerm::ServerAdmin) {
            return vec![ServerPerm::ServerAdmin];
        }
        ServerPerm::ALL
            .into_iter()
            .filter(|p| self.has(*p))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("*", "anything"));
        assert!(glob("*", ""));
        assert!(glob("wiki", "wiki"));
        assert!(!glob("wiki", "wiki2"));
        assert!(glob("wiki-*", "wiki-"));
        assert!(glob("wiki-*", "wiki-sandbox"));
        assert!(!glob("wiki-*", "wiki"));
        assert!(glob("team-*-prod", "team-a-prod"));
        assert!(glob("team-*-prod", "team--prod"));
        assert!(!glob("team-*-prod", "team-a-dev"));
        assert!(glob("*a*b*", "xxaxxbxx"));
        assert!(!glob("*a*b*", "xxbxxaxx"));
        assert!(!glob("Wiki", "wiki"));
        assert!(glob("a**", "abc"));
    }

    #[test]
    fn levels_union_and_server_admin() {
        let g = Grants {
            datasets: vec![
                ("wiki".into(), Level::Write),
                ("team-*".into(), Level::Read),
                ("*".into(), Level::Read),
                ("team-a".into(), Level::Admin),
            ],
            server: vec![ServerPerm::Metrics],
        };
        assert_eq!(g.level("wiki"), Some(Level::Write));
        assert_eq!(g.level("team-a"), Some(Level::Admin));
        assert_eq!(g.level("team-b"), Some(Level::Read));
        assert_eq!(g.level("other"), Some(Level::Read));
        assert!(g.has(ServerPerm::Metrics));
        assert!(!g.has(ServerPerm::Federate));
        let none = Grants::default();
        assert_eq!(none.level("wiki"), None);
        let admin = Grants {
            datasets: vec![],
            server: vec![ServerPerm::ServerAdmin],
        };
        assert_eq!(admin.level("anything"), Some(Level::Admin));
        assert!(admin.has(ServerPerm::Federate));
        let p = Principal::local();
        let bob = Access {
            grants: g.clone(),
            scopes: vec![Scope {
                datasets: [("team-*".to_string(), Level::Admin)].into(),
                server: vec![],
            }],
        };
        assert_eq!(bob.level("team-a"), Some(Level::Admin));
        assert_eq!(bob.level("team-b"), Some(Level::Read));
        assert_eq!(bob.level("wiki"), None);
        assert!(!bob.has(ServerPerm::Metrics));
        let all = Access {
            grants: g.clone(),
            scopes: vec![Scope::all(), Scope::all()],
        };
        assert_eq!(all.level("wiki"), Some(Level::Write));
        assert!(all.has(ServerPerm::Metrics));
        assert!(!all.has(ServerPerm::Federate));
        // server-admin in a scope does not widen its datasets
        let narrow = Access {
            grants: admin.clone(),
            scopes: vec![Scope {
                datasets: [("wiki".to_string(), Level::Read)].into(),
                server: vec!["server-admin".into()],
            }],
        };
        assert_eq!(narrow.level("wiki"), Some(Level::Read));
        assert_eq!(narrow.level("other"), None);
        assert!(narrow.has(ServerPerm::Metrics));
        assert!(p.can("x", Level::Admin));
        assert_eq!(p.access("x"), None);
        assert_eq!(p.log_name(), None);
    }
}
