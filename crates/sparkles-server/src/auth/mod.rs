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
    PrincipalKeyer, cors_layer, flush, load, proxy_host_warning, public_url, render_metrics,
    restrict, routes, server_json, spawn_reload_on_sighup, throttle,
};
pub use proxy::Peer;
#[cfg(feature = "mcp")]
pub use routes::authentication_required;
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
    sparkles::access::glob(pattern, name)
}

/// A service of a dataset that a grant may be limited to (Fuseki's operation names,
/// and a few of Sparkles').
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Endpoint {
    /// SPARQL queries, explain, `/{ds}/text`, `/{ds}/geo`, the MCP query tools
    Query,
    /// SPARQL Update, the MCP update tool
    Update,
    /// Graph Store reads
    GspR,
    /// Graph Store reads and writes
    GspRw,
    Upload,
    Shacl,
    Shex,
    Diff,
    /// every other route that reads (or, for prefixes, writes) the dataset's description
    Info,
}

impl Endpoint {
    pub const ALL: [Endpoint; 9] = [
        Endpoint::Query,
        Endpoint::Update,
        Endpoint::GspR,
        Endpoint::GspRw,
        Endpoint::Upload,
        Endpoint::Shacl,
        Endpoint::Shex,
        Endpoint::Diff,
        Endpoint::Info,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Endpoint::Query => "query",
            Endpoint::Update => "update",
            Endpoint::GspR => "gsp-r",
            Endpoint::GspRw => "gsp-rw",
            Endpoint::Upload => "upload",
            Endpoint::Shacl => "shacl",
            Endpoint::Shex => "shex",
            Endpoint::Diff => "diff",
            Endpoint::Info => "info",
        }
    }

    pub fn parse(s: &str) -> Option<Endpoint> {
        Endpoint::ALL.into_iter().find(|e| e.as_str() == s)
    }

    /// Whether a grant for this endpoint covers a request to `e` (`gsp-rw` includes
    /// the reads of `gsp-r`, as in Fuseki).
    pub fn covers(self, e: Endpoint) -> bool {
        self == e || self == Endpoint::GspRw && e == Endpoint::GspR
    }
}

/// A dataset grant limited to some graphs, some endpoints, or both (never `admin`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restricted {
    /// dataset name or `*` pattern
    pub dataset: String,
    pub level: Level,
    /// graph names and IRI patterns; `None` is every graph
    pub graphs: Option<Vec<String>>,
    /// `None` is every endpoint
    pub endpoints: Option<Vec<Endpoint>>,
    /// the protections it lifts in its graphs, at its level
    pub lifts: Vec<String>,
}

impl Restricted {
    fn applies(&self, ds: &str, e: Option<Endpoint>) -> bool {
        glob(&self.dataset, ds)
            && match (e, &self.endpoints) {
                (_, None) | (None, Some(_)) => true,
                (Some(e), Some(es)) => es.iter().any(|x| x.covers(e)),
            }
    }

    fn graphs(&self) -> sparkles::access::Graphs {
        match &self.graphs {
            None => sparkles::access::Graphs::All,
            Some(g) => sparkles::access::Graphs::Only(sparkles::access::GraphRule::new(
                g,
                &[INFERRED_GRAPH],
            )),
        }
    }
}

/// The graph of materialized inferences: covered only by grants that name it exactly.
pub const INFERRED_GRAPH: &str = crate::http::INFERRED_GRAPH;

/// The protections of a policy (`[[protections]]`), by dataset pattern, and the limits
/// on applying them.
#[derive(Debug, PartialEq, Eq)]
pub struct Protections {
    pub list: Vec<(String, Arc<sparkles::access::Protection>)>,
    pub limits: sparkles::access::Limits,
}

/// Dataset grants by pattern and server permissions; roles are already flattened in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Grants {
    pub datasets: Vec<(String, Level)>,
    pub server: Vec<ServerPerm>,
    /// grants limited to some graphs or endpoints, or lifting protections
    pub restricted: Vec<Restricted>,
    /// the names of the roles flattened in (a protection's pattern may use them)
    pub roles: Vec<String>,
    /// the policy's protections (`None`: there are none)
    pub protections: Option<Arc<Protections>>,
}

impl Grants {
    /// The highest level any matching pattern grants (independent of whether the
    /// dataset exists); `admin` everywhere with `server-admin`.
    #[cfg(test)]
    pub fn level(&self, ds: &str) -> Option<Level> {
        self.level_for(ds, None)
    }

    /// [`level`](Self::level) through endpoint `e` (any endpoint when `None`).
    pub fn level_for(&self, ds: &str, e: Option<Endpoint>) -> Option<Level> {
        if self.server.contains(&ServerPerm::ServerAdmin) {
            return Some(Level::Admin);
        }
        let full = self
            .datasets
            .iter()
            .filter(|(p, _)| glob(p, ds))
            .map(|(_, l)| *l);
        let restricted = self
            .restricted
            .iter()
            .filter(|r| r.applies(ds, e))
            .map(|r| r.level);
        full.chain(restricted).max()
    }

    /// The graphs of `ds` that the grants of at least `min` reach through endpoint `e`:
    /// a union, where a grant without a graph list covers every graph.
    pub fn graphs(&self, ds: &str, e: Endpoint, min: Level) -> sparkles::access::Graphs {
        use sparkles::access::Graphs;
        if self.server.contains(&ServerPerm::ServerAdmin)
            || self.datasets.iter().any(|(p, l)| *l >= min && glob(p, ds))
        {
            return Graphs::All;
        }
        let mut g = Graphs::none();
        for r in &self.restricted {
            if r.level >= min && r.applies(ds, Some(e)) {
                g = g.union(&r.graphs());
                if g.is_all() {
                    break;
                }
            }
        }
        g
    }

    pub fn has(&self, p: ServerPerm) -> bool {
        self.server.contains(&ServerPerm::ServerAdmin) || self.server.contains(&p)
    }

    /// The graphs of `ds` where the grants of at least `min` reach through endpoint `e`
    /// and lift the protection `name`: a union.
    fn lifted(&self, ds: &str, e: Endpoint, min: Level, name: &str) -> sparkles::access::Graphs {
        let mut g = sparkles::access::Graphs::none();
        for r in &self.restricted {
            if r.level >= min && r.applies(ds, Some(e)) && r.lifts.iter().any(|x| x == name) {
                g = g.union(&r.graphs());
                if g.is_all() {
                    break;
                }
            }
        }
        g
    }

    /// The protections of `ds` as they apply to these grants through endpoint `e` at
    /// level `l` (`None` when the policy has none for `ds`, or `admin` lifts them all).
    pub fn triple_rules(
        &self,
        ds: &str,
        e: Endpoint,
        l: Level,
        caller: sparkles::access::Caller,
    ) -> Option<sparkles::access::TripleRules> {
        let ps = self.protections.as_ref()?;
        let mine: Vec<&Arc<sparkles::access::Protection>> = ps
            .list
            .iter()
            .filter(|(d, _)| glob(d, ds))
            .map(|(_, p)| p)
            .collect();
        // admin acts on the whole dataset, as it does on every graph
        if mine.is_empty() || self.level_for(ds, None) == Some(Level::Admin) {
            return None;
        }
        let rules = mine
            .into_iter()
            .map(|p| sparkles::access::Rule {
                protection: p.clone(),
                read: self.lifted(ds, e, Level::Read, &p.name),
                write: if l >= Level::Write {
                    self.lifted(ds, e, Level::Write, &p.name)
                } else {
                    sparkles::access::Graphs::none()
                },
            })
            .collect();
        Some(sparkles::access::TripleRules {
            rules,
            caller,
            limits: ps.limits,
        })
    }

    /// Add `other`'s grants.
    pub fn extend(&mut self, other: &Grants) {
        for r in &other.roles {
            if !self.roles.contains(r) {
                self.roles.push(r.clone());
            }
        }
        if self.protections.is_none() {
            self.protections = other.protections.clone();
        }
        for g in &other.datasets {
            if !self.datasets.contains(g) {
                self.datasets.push(g.clone());
            }
        }
        for r in &other.restricted {
            if !self.restricted.contains(r) {
                self.restricted.push(r.clone());
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
        self.level_for(ds, None)
    }

    /// [`level`](Self::level) through endpoint `e` (any endpoint when `None`).
    pub fn level_for(&self, ds: &str, e: Option<Endpoint>) -> Option<Level> {
        let mut l = self.grants.level_for(ds, e)?;
        for s in &self.scopes {
            l = l.min(s.level(ds)?);
        }
        Some(l)
    }

    /// The graph view of `ds` through endpoint `e`: `None` when it covers every graph
    /// the level allows (or the dataset is not granted at all), and no graph when other
    /// endpoints are granted but not `e`.
    /// Whether the grants show only some graphs of `ds` through endpoint `e` (for
    /// reading, or for writing at a level that writes).
    pub fn graphs_limited(&self, ds: &str, e: Endpoint) -> bool {
        let Some(l) = self.level_for(ds, Some(e)) else {
            return self.level_for(ds, None).is_some();
        };
        !(self.grants.graphs(ds, e, Level::Read).is_all()
            && (l < Level::Write || self.grants.graphs(ds, e, Level::Write).is_all()))
    }

    pub fn view(
        &self,
        ds: &str,
        e: Endpoint,
        caller: sparkles::access::Caller,
    ) -> Option<sparkles::access::GraphAccess> {
        let Some(l) = self.level_for(ds, Some(e)) else {
            self.level_for(ds, None)?;
            return Some(sparkles::access::GraphAccess::graphs(
                sparkles::access::Graphs::none(),
                sparkles::access::Graphs::none(),
            ));
        };
        let read = self.grants.graphs(ds, e, Level::Read);
        let write = if l >= Level::Write {
            self.grants.graphs(ds, e, Level::Write)
        } else {
            sparkles::access::Graphs::none()
        };
        let graphs_all = read.is_all() && (l < Level::Write || write.is_all());
        let a = match self.grants.triple_rules(ds, e, l, caller) {
            Some(rules) => sparkles::access::GraphAccess::with_triples(read, write, rules),
            None if graphs_all => return None,
            None => sparkles::access::GraphAccess::graphs(read, write),
        };
        if graphs_all && a.triples.is_none() {
            return None;
        }
        Some(a)
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

/// How a principal's grants limit one dataset (`whoami`).
pub struct Limits {
    /// some endpoint sees only some graphs
    pub graphs: bool,
    /// protections hide some triples or keep them from being written
    pub triples: bool,
    /// the endpoints it may use, when not all of them
    pub endpoints: Option<Vec<Endpoint>>,
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
                server: vec![ServerPerm::ServerAdmin],
                ..Default::default()
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

    /// The level granted on `ds` through endpoint `e`.
    pub fn level_at(&self, ds: &str, e: Endpoint) -> Option<Level> {
        self.access.level_for(ds, Some(e))
    }

    /// Whether `need` is granted on `ds` through endpoint `e`.
    pub fn can_at(&self, ds: &str, e: Endpoint, need: Level) -> bool {
        self.level_at(ds, e).is_some_and(|l| l >= need)
    }

    /// The graphs of `ds` this principal reads and writes through endpoint `e`, when
    /// its grants do not cover every graph (`None` otherwise, and always without auth).
    pub fn view(&self, ds: &str, e: Endpoint) -> Option<Arc<sparkles::access::GraphAccess>> {
        if self.is_local() {
            return None;
        }
        self.access.view(ds, e, self.caller()).map(Arc::new)
    }

    /// What a protection's pattern may know of this caller: its name (the owner's for
    /// a minted token, none when anonymous), its roles and its groups.
    pub fn caller(&self) -> sparkles::access::Caller {
        let user = match self.kind {
            Kind::Local | Kind::Anonymous => None,
            Kind::Token => Some(
                self.info
                    .owner
                    .as_ref()
                    .map_or_else(|| self.name.to_string(), |o| o.name.clone()),
            ),
            _ => Some(self.name.to_string()),
        };
        let mut roles = self.access.grants.roles.clone();
        roles.sort();
        sparkles::access::Caller {
            user,
            roles,
            groups: self
                .info
                .owner
                .as_ref()
                .map(|o| o.groups.clone())
                .unwrap_or_default(),
        }
    }

    /// Whether some endpoint of `ds` shows this principal only some of its graphs.
    pub fn restricted(&self, ds: &str) -> bool {
        !self.is_local()
            && Endpoint::ALL
                .into_iter()
                .any(|e| self.view(ds, e).is_some())
    }

    /// How this principal's grants limit `ds`, for `whoami`: whether some endpoint sees
    /// only some graphs, and the endpoints it may use when not all of them. `None` when
    /// nothing is limited.
    pub fn limits(&self, ds: &str) -> Option<Limits> {
        if self.is_local() {
            return None;
        }
        let allowed: Vec<Endpoint> = Endpoint::ALL
            .into_iter()
            .filter(|e| self.can_at(ds, *e, Level::Read))
            .collect();
        let views: Vec<_> = allowed.iter().filter_map(|e| self.view(ds, *e)).collect();
        let graphs = allowed.iter().any(|e| self.access.graphs_limited(ds, *e));
        let triples = views.iter().any(|v| v.triples.is_some());
        let endpoints = (allowed.len() < Endpoint::ALL.len()).then_some(allowed);
        (graphs || triples || endpoints.is_some()).then_some(Limits {
            graphs,
            triples,
            endpoints,
        })
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
            ..Default::default()
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
            ..Default::default()
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
