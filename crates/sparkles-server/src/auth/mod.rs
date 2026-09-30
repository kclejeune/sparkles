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
mod routes;

#[cfg(feature = "auth")]
pub mod cli;
#[cfg(feature = "auth")]
pub mod config;
#[cfg(feature = "auth")]
pub mod crypto;
#[cfg(feature = "auth")]
mod policy;

pub use api::{
    cors_layer, load, render_metrics, restrict, server_json, spawn_reload_on_sighup, whoami,
};
pub use routes::{AuthReport, Denied, forbidden, middleware};
#[cfg(test)]
pub use routes::{ROUTES, need};

#[cfg(feature = "auth")]
pub use policy::Auth;
#[cfg(all(test, feature = "auth"))]
pub use policy::{hash_password, hash_password_with, new_token, token_hash};

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

/// What kind of caller a principal is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// auth is disabled: everything is allowed
    Local,
    Anonymous,
    /// a configured user (HTTP Basic)
    User,
    /// an API token
    Token,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Local => "local",
            Kind::Anonymous => "anonymous",
            Kind::User => "user",
            Kind::Token => "token",
        }
    }
}

/// How the request authenticated (the access log's `auth` field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    None,
    Basic,
    Bearer,
}

impl Scheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::None => "none",
            Scheme::Basic => "basic",
            Scheme::Bearer => "bearer",
        }
    }
}

/// The caller of a request, inserted into the request extensions by [`middleware`].
#[derive(Clone)]
pub struct Principal {
    pub kind: Kind,
    pub name: Arc<str>,
    pub scheme: Scheme,
    grants: Arc<Grants>,
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
            grants: Arc::new(Grants {
                datasets: Vec::new(),
                server: vec![ServerPerm::ServerAdmin],
            }),
        });
        LOCAL.clone()
    }

    pub fn new(kind: Kind, name: &str, scheme: Scheme, grants: Arc<Grants>) -> Principal {
        Principal {
            kind,
            name: name.into(),
            scheme,
            grants,
        }
    }

    pub fn is_local(&self) -> bool {
        self.kind == Kind::Local
    }

    pub fn is_anonymous(&self) -> bool {
        self.kind == Kind::Anonymous
    }

    /// The level granted on dataset `ds` (whether or not it exists).
    pub fn level(&self, ds: &str) -> Option<Level> {
        self.grants.level(ds)
    }

    pub fn can(&self, ds: &str, need: Level) -> bool {
        self.level(ds).is_some_and(|l| l >= need)
    }

    /// `server-admin` implies every server permission.
    pub fn has(&self, p: ServerPerm) -> bool {
        self.grants.has(p)
    }

    /// `anonymous`, `user:bob`, `token:etl`; `None` when auth is disabled.
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
        ServerPerm::ALL
            .into_iter()
            .filter(|p| {
                if self.grants.server.contains(&ServerPerm::ServerAdmin) {
                    *p == ServerPerm::ServerAdmin
                } else {
                    self.grants.server.contains(p)
                }
            })
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
        assert!(p.can("x", Level::Admin));
        assert_eq!(p.access("x"), None);
        assert_eq!(p.log_name(), None);
    }
}
