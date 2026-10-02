//! The auth configuration file (TOML): parsing, validation and warnings.

use super::{Level, ServerPerm};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// `[anonymous]`, `[roles.NAME]`: grants without roles.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantsCfg {
    #[serde(default)]
    pub datasets: BTreeMap<String, Level>,
    #[serde(default)]
    pub server: Vec<ServerPerm>,
    #[serde(default)]
    pub grants: Vec<GrantCfg>,
}

/// `[[….grants]]`: a dataset grant limited to some graphs or endpoints.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantCfg {
    /// a dataset name or `*` pattern
    pub dataset: String,
    /// `read` or `write`
    pub level: Level,
    /// graph IRIs, IRI patterns with `*`, and `urn:x-arq:DefaultGraph` (or `default`);
    /// absent: every graph
    #[serde(default)]
    pub graphs: Option<Vec<String>>,
    /// endpoint names; absent: every endpoint
    #[serde(default)]
    pub endpoints: Option<Vec<String>>,
}

impl GrantCfg {
    /// The grant, once [`check_restricted`] has accepted it.
    pub fn restricted(&self) -> super::Restricted {
        super::Restricted {
            dataset: self.dataset.clone(),
            level: self.level,
            graphs: self.graphs.clone(),
            endpoints: self.endpoints.as_ref().map(|es| {
                es.iter()
                    .filter_map(|e| super::Endpoint::parse(e))
                    .collect()
            }),
        }
    }
}

/// A graph entry of a grant: the default graph's name, or an absolute IRI that may
/// contain `*`.
fn valid_graph_name(g: &str) -> bool {
    if matches!(g, "default" | sparkles::sparql::ctx::DEFAULT_GRAPH_IRI) || g == "*" {
        return true;
    }
    if g == sparkles::sparql::ctx::UNION_GRAPH_IRI {
        return false;
    }
    // with every `*` replaced, the rest must be an absolute IRI
    oxrdf::NamedNode::new(g.replace('*', "x")).is_ok()
}

/// Validate the restricted grants of a grantee.
fn check_restricted(what: &str, grants: &[GrantCfg]) -> Result<()> {
    for g in grants {
        if !valid_pattern(&g.dataset) {
            bail!("{what}: invalid dataset pattern '{}' in grants", g.dataset);
        }
        if g.level == Level::Admin {
            bail!(
                "{what}: a grant on '{}' cannot be admin (admin covers every graph and \
                 endpoint: grant it under datasets)",
                g.dataset
            );
        }
        if let Some(gs) = &g.graphs {
            if gs.is_empty() {
                bail!(
                    "{what}: the grant on '{}' has an empty graphs list (omit graphs to cover \
                     every graph)",
                    g.dataset
                );
            }
            for x in gs {
                if x == sparkles::sparql::ctx::UNION_GRAPH_IRI {
                    bail!(
                        "{what}: the grant on '{}' names {x}, which is not a graph (list the \
                         graphs, or a pattern such as *)",
                        g.dataset
                    );
                }
                if !valid_graph_name(x) {
                    bail!(
                        "{what}: the grant on '{}' has an invalid graph '{x}' (expected an \
                         absolute IRI, a pattern with *, or urn:x-arq:DefaultGraph)",
                        g.dataset
                    );
                }
            }
        }
        if let Some(es) = &g.endpoints {
            if es.is_empty() {
                bail!(
                    "{what}: the grant on '{}' has an empty endpoints list (omit endpoints to \
                     cover every endpoint)",
                    g.dataset
                );
            }
            for e in es {
                if super::Endpoint::parse(e).is_none() {
                    let known: Vec<&str> =
                        super::Endpoint::ALL.iter().map(|e| e.as_str()).collect();
                    bail!(
                        "{what}: the grant on '{}' names an unknown endpoint '{e}' (known: {})",
                        g.dataset,
                        known.join(", ")
                    );
                }
            }
        }
    }
    Ok(())
}

/// Warnings about the restricted grants of a grantee: a restriction that a grant of the
/// same grantee lifts, and a wildcard that leaves the inferred graph out.
fn restricted_warnings(
    what: &str,
    datasets: &BTreeMap<String, Level>,
    grants: &[GrantCfg],
    out: &mut Vec<String>,
) {
    for g in grants {
        if datasets
            .iter()
            .any(|(p, l)| *l >= g.level && (p == "*" || p == &g.dataset))
        {
            out.push(format!(
                "{what}: the restricted {} grant on '{}' has no effect, since datasets already \
                 grants it on every graph and endpoint",
                g.level.as_str(),
                g.dataset
            ));
        }
        if let Some(gs) = &g.graphs
            && gs
                .iter()
                .any(|x| x.contains('*') && super::glob(x, super::INFERRED_GRAPH))
            && !gs.iter().any(|x| x == super::INFERRED_GRAPH)
        {
            out.push(format!(
                "{what}: a pattern of the grant on '{}' would match {}, which only an exact \
                 name covers (it holds inferences from every graph)",
                g.dataset,
                super::INFERRED_GRAPH
            ));
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserCfg {
    pub name: String,
    /// argon2id PHC string (`sparkles auth hash`)
    pub password: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub datasets: BTreeMap<String, Level>,
    #[serde(default)]
    pub server: Vec<ServerPerm>,
    #[serde(default)]
    pub grants: Vec<GrantCfg>,
}

impl std::fmt::Debug for UserCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserCfg")
            .field("name", &self.name)
            .field("password", &"<redacted>")
            .field("roles", &self.roles)
            .field("datasets", &self.datasets)
            .field("server", &self.server)
            .field("grants", &self.grants)
            .finish()
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenCfg {
    pub name: String,
    /// `sha256:<64 lowercase hex>` of the whole token string
    pub hash: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub datasets: BTreeMap<String, Level>,
    #[serde(default)]
    pub server: Vec<ServerPerm>,
    #[serde(default)]
    pub grants: Vec<GrantCfg>,
    /// RFC 3339
    #[serde(default)]
    pub expires: Option<String>,
}

impl std::fmt::Debug for TokenCfg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenCfg")
            .field("name", &self.name)
            .field("hash", &"<redacted>")
            .field("roles", &self.roles)
            .field("datasets", &self.datasets)
            .field("server", &self.server)
            .field("grants", &self.grants)
            .field("expires", &self.expires)
            .finish()
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorsCfg {
    #[serde(default)]
    pub origins: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerCfg {
    /// the server's external URL (`https://sparql.example.org`): the OIDC redirect URI,
    /// cookie attributes and own origin
    #[serde(default)]
    pub public_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokensPolicyCfg {
    #[serde(default = "default_ttl")]
    pub default_ttl: String,
    #[serde(default = "max_ttl")]
    pub max_ttl: String,
    /// unexpired minted tokens per owner at most
    #[serde(default = "max_active_per_owner")]
    pub max_active_per_owner: usize,
    /// tokens an owner may mint: `N/s|min|h|d[,burst=N]`, or `off`
    #[serde(default = "mint_rate")]
    pub mint_rate: String,
}

impl Default for TokensPolicyCfg {
    fn default() -> Self {
        TokensPolicyCfg {
            default_ttl: default_ttl(),
            max_ttl: max_ttl(),
            max_active_per_owner: max_active_per_owner(),
            mint_rate: mint_rate(),
        }
    }
}

fn max_active_per_owner() -> usize {
    100
}
fn mint_rate() -> String {
    "60/h".into()
}

/// `tokens_policy.mint_rate`: a rate with an optional burst, or `off`.
pub fn parse_mint_rate(s: &str) -> Result<crate::ratelimit::Limit> {
    let l = crate::ratelimit::Limit::parse(s)
        .map_err(|e| anyhow::anyhow!("tokens_policy.mint_rate: {e}"))?;
    if l.concurrency.is_some() || l.client_concurrency.is_some() || l.failure_cost.is_some() {
        bail!("tokens_policy.mint_rate: expected N/s, N/min, N/h or N/d [,burst=N] or off");
    }
    Ok(l)
}

fn default_ttl() -> String {
    "30d".into()
}
fn max_ttl() -> String {
    "90d".into()
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcCfg {
    pub issuer: String,
    pub client_id: String,
    /// a file holding the client secret; absent for a public client (PKCE only)
    #[serde(default)]
    pub client_secret_file: Option<String>,
    #[serde(default = "default_scopes")]
    pub scopes: Vec<String>,
    /// `email`, `preferred_username` or `sub`
    #[serde(default = "default_name_claim")]
    pub name_claim: String,
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,
    #[serde(default = "default_display_name")]
    pub display_name: String,
    #[serde(default = "default_algorithms")]
    pub algorithms: Vec<String>,
}

fn default_scopes() -> Vec<String> {
    vec!["openid".into(), "profile".into(), "email".into()]
}
fn default_name_claim() -> String {
    "email".into()
}
fn default_groups_claim() -> String {
    "groups".into()
}
fn default_display_name() -> String {
    "single sign-on".into()
}
fn default_algorithms() -> Vec<String> {
    vec!["RS256".into(), "ES256".into()]
}

/// Admission and role mapping of OIDC and proxy identities.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalCfg {
    #[serde(default)]
    pub allowed_users: Vec<String>,
    #[serde(default)]
    pub allowed_groups: Vec<String>,
    #[serde(default)]
    pub default_roles: Vec<String>,
    #[serde(default)]
    pub group_roles: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub user_roles: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCfg {
    #[serde(default = "session_ttl")]
    pub ttl: String,
    /// default `<data>/auth/session.key`, created on first start
    #[serde(default)]
    pub key_file: Option<String>,
}

impl Default for SessionCfg {
    fn default() -> Self {
        SessionCfg {
            ttl: session_ttl(),
            key_file: None,
        }
    }
}

fn session_ttl() -> String {
    "12h".into()
}

/// Trusted-header authentication behind a forward-auth proxy.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyCfg {
    /// `oauth2-proxy`, `authelia`, `tailscale` or `cloudflare-access`
    #[serde(default)]
    pub preset: Option<String>,
    /// CIDRs of the proxy, and `unix` for the Unix socket
    pub trusted: Vec<String>,
    #[serde(default)]
    pub user_header: Option<String>,
    #[serde(default)]
    pub email_header: Option<String>,
    #[serde(default)]
    pub groups_header: Option<String>,
    #[serde(default = "default_separator")]
    pub groups_separator: String,
    /// `user` or `email`: which header names the principal
    #[serde(default = "default_name_from")]
    pub name_from: String,
    #[serde(default)]
    pub logout_url: Option<String>,
}

fn default_separator() -> String {
    ",".into()
}
fn default_name_from() -> String {
    "user".into()
}

/// Header names of a proxy preset: user, email, groups.
pub fn proxy_preset(name: &str) -> Option<(&'static str, &'static str, Option<&'static str>)> {
    Some(match name {
        "oauth2-proxy" => (
            "X-Forwarded-User",
            "X-Forwarded-Email",
            Some("X-Forwarded-Groups"),
        ),
        "authelia" => ("Remote-User", "Remote-Email", Some("Remote-Groups")),
        "tailscale" => ("Tailscale-User-Login", "Tailscale-User-Login", None),
        "cloudflare-access" => (
            "Cf-Access-Authenticated-User-Email",
            "Cf-Access-Authenticated-User-Email",
            None,
        ),
        _ => return None,
    })
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    pub version: u32,
    #[serde(default = "default_realm")]
    pub realm: String,
    #[serde(default)]
    pub server: ServerCfg,
    #[serde(default)]
    pub anonymous: GrantsCfg,
    #[serde(default)]
    pub roles: BTreeMap<String, GrantsCfg>,
    #[serde(default)]
    pub users: Vec<UserCfg>,
    #[serde(default)]
    pub tokens: Vec<TokenCfg>,
    #[serde(default)]
    pub tokens_policy: TokensPolicyCfg,
    #[serde(default)]
    pub oidc: Option<OidcCfg>,
    #[serde(default)]
    pub external: ExternalCfg,
    #[serde(default)]
    pub session: SessionCfg,
    #[serde(default)]
    pub proxy: Option<ProxyCfg>,
    #[serde(default)]
    pub cors: CorsCfg,
}

fn default_realm() -> String {
    "sparkles".into()
}

/// OWASP's minimum argon2id parameters (m in KiB).
pub const OWASP_M: u32 = 19456;
pub const OWASP_T: u32 = 2;
pub const OWASP_P: u32 = 1;

/// User, token and role names: `[A-Za-z0-9_.@-]{1,64}`.
pub fn valid_principal_name(n: &str) -> bool {
    (1..=64).contains(&n.len())
        && n.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'-'))
}

/// A dataset pattern: a dataset name that may contain `*`.
pub fn valid_pattern(p: &str) -> bool {
    p == "*" || crate::state::valid_name(&p.replace('*', "x")) && !p.starts_with('.')
}

/// Parse an RFC 3339 timestamp into Unix seconds.
pub fn parse_rfc3339(s: &str) -> Result<i64> {
    Ok(chrono::DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("invalid RFC 3339 timestamp '{s}'"))?
        .timestamp())
}

/// A duration `<n>s|m|h|d` in seconds.
pub fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim();
    let (n, unit) = s.split_at(s.len().saturating_sub(1));
    let mult = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => bail!("invalid duration '{s}' (expected <n>s, <n>m, <n>h or <n>d)"),
    };
    let n: i64 =
        n.parse().ok().filter(|n| *n > 0).with_context(|| {
            format!("invalid duration '{s}' (expected <n>s, <n>m, <n>h or <n>d)")
        })?;
    n.checked_mul(mult)
        .with_context(|| format!("duration '{s}' is too long"))
}

/// A public URL: `http(s)://host[:port]` without a path; `http` only for loopback hosts.
pub fn check_public_url(u: &str) -> Result<()> {
    let url = reqwest::Url::parse(u)
        .with_context(|| format!("server.public_url '{u}' is not an absolute URL"))?;
    if !matches!(url.path(), "" | "/") || url.query().is_some() || !url.username().is_empty() {
        bail!("server.public_url '{u}' must be scheme://host[:port] without a path");
    }
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match url.scheme() {
        "https" => Ok(()),
        "http" if loopback => Ok(()),
        _ => bail!("server.public_url must use https (http is allowed only for localhost)"),
    }
}

/// `sha256:<64 lowercase hex>` → the digest.
pub fn parse_token_hash(h: &str) -> Option<[u8; 32]> {
    let hex = h.strip_prefix("sha256:")?;
    if hex.len() != 64 || !hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// argon2 parameters `(m, t, p)` of a PHC string, if it is argon2id.
pub fn argon2id_params(phc: &str) -> Option<(u32, u32, u32)> {
    let rest = phc.strip_prefix("$argon2id$")?;
    let mut parts = rest.split('$');
    let first = parts.next()?;
    let params = if first.starts_with("v=") {
        parts.next()?
    } else {
        first
    };
    let (mut m, mut t, mut p) = (None, None, None);
    for kv in params.split(',') {
        let (k, v) = kv.split_once('=')?;
        let v: u32 = v.parse().ok()?;
        match k {
            "m" => m = Some(v),
            "t" => t = Some(v),
            "p" => p = Some(v),
            _ => {}
        }
    }
    Some((m?, t?, p?))
}

fn check_grants(
    what: &str,
    datasets: &BTreeMap<String, Level>,
    roles: &[String],
    known_roles: &BTreeMap<String, GrantsCfg>,
) -> Result<()> {
    for pat in datasets.keys() {
        if !valid_pattern(pat) {
            bail!("{what}: invalid dataset pattern '{pat}'");
        }
    }
    for r in roles {
        if !known_roles.contains_key(r) {
            bail!("{what}: unknown role '{r}'");
        }
    }
    Ok(())
}

impl FileConfig {
    /// Parse (errors carry the line and column) and validate.
    pub fn parse(text: &str) -> Result<FileConfig> {
        let cfg: FileConfig = toml::from_str(text).map_err(|e| anyhow::anyhow!("{e}"))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Read and parse `path`; the returned warnings include the file mode check.
    pub fn load(path: &Path) -> Result<(FileConfig, Vec<String>)> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the auth configuration {}", path.display()))?;
        let cfg = FileConfig::parse(&text)
            .with_context(|| format!("invalid auth configuration {}", path.display()))?;
        let mut warnings = cfg.warnings();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(m) = std::fs::metadata(path)
                && m.permissions().mode() & 0o077 != 0
            {
                warnings.push(format!(
                    "{} is readable by group or others (mode {:o}); use 0600 or 0640",
                    path.display(),
                    m.permissions().mode() & 0o777
                ));
            }
        }
        Ok((cfg, warnings))
    }

    fn validate(&self) -> Result<()> {
        if self.version != 1 {
            bail!("version must be 1");
        }
        if self.realm.is_empty() || self.realm.contains(['"', '\\']) || !self.realm.is_ascii() {
            bail!("realm must be printable ASCII without quotes or backslashes");
        }
        for name in self.roles.keys() {
            if !valid_principal_name(name) {
                bail!("invalid role name '{name}'");
            }
        }
        check_grants("[anonymous]", &self.anonymous.datasets, &[], &self.roles)?;
        check_restricted("[anonymous]", &self.anonymous.grants)?;
        for (name, r) in &self.roles {
            check_grants(&format!("role {name}"), &r.datasets, &[], &self.roles)?;
            check_restricted(&format!("role {name}"), &r.grants)?;
        }
        let mut names = BTreeSet::new();
        for u in &self.users {
            if !valid_principal_name(&u.name) {
                bail!("invalid user name '{}'", u.name);
            }
            if !names.insert(u.name.as_str()) {
                bail!("duplicate user '{}'", u.name);
            }
            if argon2id_params(&u.password).is_none() {
                bail!(
                    "user {}: password must be an argon2id PHC string ($argon2id$…, see `sparkles auth hash`)",
                    u.name
                );
            }
            check_grants(
                &format!("user {}", u.name),
                &u.datasets,
                &u.roles,
                &self.roles,
            )?;
            check_restricted(&format!("user {}", u.name), &u.grants)?;
        }
        let mut names = BTreeSet::new();
        let mut hashes = BTreeSet::new();
        for t in &self.tokens {
            if !valid_principal_name(&t.name) {
                bail!("invalid token name '{}'", t.name);
            }
            if !names.insert(t.name.as_str()) {
                bail!("duplicate token '{}'", t.name);
            }
            if parse_token_hash(&t.hash).is_none() {
                bail!(
                    "token {}: hash must be sha256: followed by 64 lowercase hex characters",
                    t.name
                );
            }
            if !hashes.insert(t.hash.as_str()) {
                bail!("token {}: duplicate hash", t.name);
            }
            if let Some(e) = &t.expires {
                parse_rfc3339(e).with_context(|| format!("token {}: expires", t.name))?;
            }
            check_grants(
                &format!("token {}", t.name),
                &t.datasets,
                &t.roles,
                &self.roles,
            )?;
            check_restricted(&format!("token {}", t.name), &t.grants)?;
        }
        for o in &self.cors.origins {
            if !crate::exposure::valid_origin(o) {
                bail!("cors.origins: '{o}' is not scheme://host[:port]");
            }
        }
        if let Some(u) = &self.server.public_url {
            check_public_url(u)?;
        }
        let dflt =
            parse_duration(&self.tokens_policy.default_ttl).context("tokens_policy.default_ttl")?;
        let max = parse_duration(&self.tokens_policy.max_ttl).context("tokens_policy.max_ttl")?;
        if max < dflt {
            bail!("tokens_policy.max_ttl must be at least default_ttl");
        }
        if self.tokens_policy.max_active_per_owner == 0 {
            bail!("tokens_policy.max_active_per_owner must be at least 1");
        }
        parse_mint_rate(&self.tokens_policy.mint_rate)?;
        parse_duration(&self.session.ttl).context("session.ttl")?;
        let ext = &self.external;
        for r in ext
            .default_roles
            .iter()
            .chain(ext.group_roles.values().flatten())
            .chain(ext.user_roles.values().flatten())
        {
            if !self.roles.contains_key(r) {
                bail!("[external]: unknown role '{r}'");
            }
        }
        if let Some(o) = &self.oidc {
            if self.server.public_url.is_none() {
                bail!("[oidc] requires server.public_url");
            }
            super::oidc::check_url("oidc.issuer", &o.issuer)?;
            if o.client_id.is_empty() {
                bail!("oidc.client_id is empty");
            }
            if !matches!(
                o.name_claim.as_str(),
                "email" | "preferred_username" | "sub"
            ) {
                bail!("oidc.name_claim must be email, preferred_username or sub");
            }
            if !o.scopes.iter().any(|s| s == "openid") {
                bail!("oidc.scopes must include openid");
            }
            for a in &o.algorithms {
                super::oidc::parse_algorithm(a)?;
            }
        }
        if let Some(p) = &self.proxy {
            if let Some(preset) = &p.preset
                && proxy_preset(preset).is_none()
            {
                bail!(
                    "proxy.preset '{preset}' is not one of oauth2-proxy, authelia, tailscale, cloudflare-access"
                );
            }
            if p.user_header.is_none() && p.preset.is_none() {
                bail!("[proxy] needs user_header or a preset");
            }
            if p.trusted.is_empty() {
                bail!("proxy.trusted is empty");
            }
            for t in &p.trusted {
                if t == "unix" {
                    continue;
                }
                let net: ipnet::IpNet = t
                    .parse()
                    .or_else(|_| t.parse::<std::net::IpAddr>().map(ipnet::IpNet::from))
                    .map_err(|_| anyhow::anyhow!("proxy.trusted: '{t}' is not a CIDR"))?;
                if net.prefix_len() == 0 {
                    bail!("proxy.trusted: '{t}' would trust every address");
                }
            }
            if !matches!(p.name_from.as_str(), "user" | "email") {
                bail!("proxy.name_from must be user or email");
            }
            if p.groups_separator.is_empty() {
                bail!("proxy.groups_separator is empty");
            }
        }
        Ok(())
    }

    /// Problems worth a WARN that do not stop the server.
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        for u in &self.users {
            if let Some((m, t, p)) = argon2id_params(&u.password)
                && (m < OWASP_M || t < OWASP_T || p < OWASP_P)
            {
                w.push(format!(
                    "user {}: argon2 parameters m={m},t={t},p={p} are below the OWASP minimum m={OWASP_M},t={OWASP_T},p={OWASP_P}",
                    u.name
                ));
            }
            if u.datasets.is_empty()
                && u.server.is_empty()
                && u.roles.is_empty()
                && u.grants.is_empty()
            {
                w.push(format!("user {} has no grants", u.name));
            }
            restricted_warnings(&format!("user {}", u.name), &u.datasets, &u.grants, &mut w);
        }
        for t in &self.tokens {
            if t.datasets.is_empty()
                && t.server.is_empty()
                && t.roles.is_empty()
                && t.grants.is_empty()
            {
                w.push(format!("token {} has no grants", t.name));
            }
            restricted_warnings(&format!("token {}", t.name), &t.datasets, &t.grants, &mut w);
        }
        for (name, r) in &self.roles {
            restricted_warnings(&format!("role {name}"), &r.datasets, &r.grants, &mut w);
        }
        restricted_warnings(
            "[anonymous]",
            &self.anonymous.datasets,
            &self.anonymous.grants,
            &mut w,
        );
        if self.anonymous.datasets.values().any(|l| *l > Level::Read)
            || self.anonymous.grants.iter().any(|g| g.level > Level::Read)
            || !self.anonymous.server.is_empty()
        {
            w.push("anonymous holds write, admin or a server permission".into());
        }
        let ext = &self.external;
        if (self.oidc.is_some() || self.proxy.is_some())
            && ext.allowed_users.is_empty()
            && ext.allowed_groups.is_empty()
            && !ext.default_roles.is_empty()
        {
            w.push(format!(
                "every account at the identity provider or proxy gets the roles {:?}",
                ext.default_roles
            ));
        }
        if let Some(p) = &self.proxy {
            for t in &p.trusted {
                if let Ok(net) = t.parse::<ipnet::IpNet>()
                    && net.prefix_len() < net.max_prefix_len()
                {
                    let header = p.user_header.clone().unwrap_or_else(|| {
                        p.preset
                            .as_deref()
                            .and_then(proxy_preset)
                            .map_or("the user header", |x| x.0)
                            .to_string()
                    });
                    w.push(format!("every host in {t} can set {header}"));
                }
            }
        }
        w
    }

    /// `3 users, 2 tokens, 1 role, anonymous: public=read`
    pub fn summary(&self) -> String {
        let plural = |n: usize, s: &str| format!("{n} {s}{}", if n == 1 { "" } else { "s" });
        let anon: Vec<String> = self
            .anonymous
            .datasets
            .iter()
            .map(|(k, v)| format!("{k}={}", v.as_str()))
            .chain(self.anonymous.server.iter().map(|s| s.as_str().to_string()))
            .collect();
        format!(
            "{}, {}, {}, anonymous: {}",
            plural(self.users.len(), "user"),
            plural(self.tokens.len(), "token"),
            plural(self.roles.len(), "role"),
            if anon.is_empty() {
                "none".to_string()
            } else {
                anon.join(", ")
            }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "sha256:6b3a55e0261b0304143f805a24924d0c1c44524821305f31d9277843b8a10f4e";

    #[test]
    fn parses_the_example() {
        let cfg = FileConfig::parse(&format!(
            r#"
version = 1
[anonymous]
datasets = {{ public = "read" }}
[roles.wiki-editors]
datasets = {{ wiki = "write", "wiki-*" = "write" }}
[[users]]
name = "alice"
password = "$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNoaGFzaA"
server = ["server-admin"]
[[users]]
name = "bob"
password = "$argon2id$v=19$m=8,t=1,p=1$c2FsdHNhbHRzYWx0$aGFzaGhhc2hoYXNoaGFzaA"
roles = ["wiki-editors"]
[[tokens]]
name = "etl"
hash = "{HASH}"
datasets = {{ wiki = "write" }}
expires = "2027-06-30T00:00:00Z"
[cors]
origins = ["https://yasgui.example.org"]
"#
        ))
        .unwrap();
        assert_eq!(cfg.users.len(), 2);
        assert_eq!(cfg.realm, "sparkles");
        let w = cfg.warnings();
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].contains("bob"));
        assert_eq!(
            cfg.summary(),
            "2 users, 1 token, 1 role, anonymous: public=read"
        );
    }

    #[test]
    fn errors_name_the_key_and_position() {
        let e = FileConfig::parse("version = 1\n[[users]]\nname = \"x\"\ndataset = {}\n")
            .unwrap_err()
            .to_string();
        assert!(e.contains("dataset"), "{e}");
        assert!(e.contains("line 4"), "{e}");
        let bad = |t: &str| FileConfig::parse(t).unwrap_err().to_string();
        assert!(bad("version = 2").contains("version"));
        assert!(bad("version = 1\n[[users]]\nname = \"a:b\"\npassword = \"$argon2id$v=19$m=8,t=1,p=1$x$y\"").contains("user name"));
        assert!(
            bad("version = 1\n[[users]]\nname = \"a\"\npassword = \"plain\"").contains("argon2id")
        );
        assert!(
            bad("version = 1\n[[users]]\nname = \"a\"\npassword = \"$argon2id$v=19$m=8,t=1,p=1$x$y\"\nroles = [\"nope\"]")
                .contains("unknown role")
        );
        assert!(
            bad("version = 1\n[[tokens]]\nname = \"a\"\nhash = \"sha256:AB\"").contains("hash")
        );
        assert!(
            bad("version = 1\n[anonymous]\ndatasets = { \"a/b\" = \"read\" }").contains("pattern")
        );
        assert!(bad("version = 1\n[anonymous]\ndatasets = { a = \"owner\" }").contains("owner"));
        assert!(bad("version = 1\n[anonymous]\nserver = [\"root\"]").contains("root"));
        assert!(bad("version = 1\n[cors]\norigins = [\"*\"]").contains("cors"));
        assert!(
            bad("version = 1\n[cors]\norigins = [\"https://a.example/path\"]").contains("cors")
        );
        assert!(
            bad(&format!(
                "version = 1\n[[tokens]]\nname = \"a\"\nhash = \"{HASH}\"\nexpires = \"tomorrow\""
            ))
            .contains("expires")
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(
            argon2id_params("$argon2id$v=19$m=19456,t=2,p=1$abc$def"),
            Some((19456, 2, 1))
        );
        assert_eq!(
            argon2id_params("$argon2i$v=19$m=19456,t=2,p=1$abc$def"),
            None
        );
        assert!(parse_token_hash(HASH).is_some());
        assert!(parse_token_hash(&HASH.to_uppercase()).is_none());
        assert!(valid_pattern("*"));
        assert!(valid_pattern("team-*"));
        assert!(valid_pattern("a*b*"));
        assert!(!valid_pattern("a b"));
        assert!(!valid_pattern(""));
        assert!(crate::exposure::valid_origin("http://localhost:5173"));
        assert!(!crate::exposure::valid_origin("localhost:5173"));
    }
}
