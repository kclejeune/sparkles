//! The MCP endpoint as an OAuth 2.1 protected resource (spec C11, item 67).
//!
//! * RFC 9728 protected-resource metadata at `/.well-known/oauth-protected-resource`
//!   and its path-suffixed form `/.well-known/oauth-protected-resource/$/mcp`. Both
//!   describe the one protected resource, `/$/mcp`, whose identifier is
//!   `server.public_url` followed by `/$/mcp`. The metadata is served only when the
//!   server runs with `--mcp` and an `[oidc]` provider; otherwise both answer `404`.
//! * The resource identifier is an accepted access-token audience at `/$/mcp` only (RFC
//!   8707). The `api_audience` values keep working on every route.
//! * Scopes `sparkles:read`, `sparkles:update` and `sparkles:admin`, each optionally
//!   followed by `:<dataset>` (a name or `*` pattern), become a [`Scope`] that narrows the
//!   principal's grants, so a scope never adds access.
//!
//! The server never reads forwarded headers to build these URLs: `server.public_url`,
//! which `[oidc]` requires, is the only source.

// the scope challenges are checked in `mcp::http`
#![cfg_attr(not(feature = "mcp"), allow(dead_code))]

use super::{Level, Scope};
use std::collections::BTreeMap;

/// The path of the protected resource.
pub const MCP_PATH: &str = "/$/mcp";
/// The path-suffixed metadata of `/$/mcp` (RFC 9728, §3.1), which `auth::api` routes
/// with the root form `/.well-known/oauth-protected-resource`.
pub const WELL_KNOWN_MCP: &str = "/.well-known/oauth-protected-resource/$/mcp";

/// The prefix every Sparkles scope starts with.
pub const PREFIX: &str = "sparkles:";
pub const READ: &str = "sparkles:read";
pub const UPDATE: &str = "sparkles:update";
pub const ADMIN: &str = "sparkles:admin";
/// `scopes_supported` of the metadata, in hierarchy order.
pub const SUPPORTED: [&str; 3] = [READ, UPDATE, ADMIN];

/// The resource identifier of `/$/mcp` under `public_url` (without a trailing slash).
pub fn resource(public_url: &str) -> String {
    format!("{}{MCP_PATH}", public_url.trim_end_matches('/'))
}

/// The URL of the path-suffixed metadata document under `public_url`.
pub fn metadata_url(public_url: &str) -> String {
    format!("{}{WELL_KNOWN_MCP}", public_url.trim_end_matches('/'))
}

/// The scope that grants `level` on every dataset.
pub fn scope_for(level: Level) -> &'static str {
    match level {
        Level::Read => READ,
        Level::Write => UPDATE,
        Level::Admin => ADMIN,
    }
}

fn level_of(name: &str) -> Option<Level> {
    Some(match name {
        "read" => Level::Read,
        "update" => Level::Write,
        "admin" => Level::Admin,
        _ => return None,
    })
}

/// The Sparkles scopes among a token's scopes.
pub fn sparkles_scopes(granted: &[String]) -> Vec<String> {
    granted
        .iter()
        .filter(|s| s.starts_with(PREFIX))
        .cloned()
        .collect()
}

/// The upper bound that Sparkles scopes put on a principal's grants. A scope that does
/// not parse adds nothing. `sparkles:admin` without a dataset also keeps every server
/// permission, and any other scope keeps only `federate`, which SERVICE in a query needs
/// on top of the dataset's grant.
pub fn scope_of(names: &[String]) -> Scope {
    let mut datasets: BTreeMap<String, Level> = BTreeMap::new();
    let mut server_all = false;
    for n in names {
        let Some(rest) = n.strip_prefix(PREFIX) else {
            continue;
        };
        let (lvl, ds) = match rest.split_once(':') {
            Some((l, ds)) => (l, ds),
            None => (rest, "*"),
        };
        let Some(l) = level_of(lvl) else { continue };
        if !valid_pattern(ds) {
            continue;
        }
        if l == Level::Admin && !rest.contains(':') {
            server_all = true;
        }
        let e = datasets.entry(ds.to_string()).or_insert(l);
        *e = (*e).max(l);
    }
    let server = if server_all {
        vec!["*".to_string()]
    } else if datasets.is_empty() {
        Vec::new()
    } else {
        vec![super::ServerPerm::Federate.as_str().to_string()]
    };
    Scope { datasets, server }
}

/// A dataset name or `*` pattern, as the auth configuration accepts it.
fn valid_pattern(p: &str) -> bool {
    p == "*" || crate::state::valid_name(&p.replace('*', "x")) && !p.starts_with('.')
}

/// A quoted-string value of an auth-param (RFC 9110 §11.2): quotes and backslashes are
/// escaped, and control characters are dropped.
fn quoted(v: &str) -> String {
    let mut s = String::with_capacity(v.len() + 2);
    s.push('"');
    for c in v.chars().filter(|c| !c.is_control()) {
        if matches!(c, '"' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('"');
    s
}

/// The `WWW-Authenticate` value of a `403` for a token whose scopes do not cover the
/// operation (RFC 6750 §3.1, and the MCP authorization spec's scope challenge).
pub fn insufficient_scope(public_url: &str, scope: &str, description: &str) -> String {
    format!(
        "Bearer error=\"insufficient_scope\", scope={}, resource_metadata={}, error_description={}",
        quoted(scope),
        quoted(&metadata_url(public_url)),
        quoted(description)
    )
}

/// The auth-params a `401` from `/$/mcp` adds to its `Bearer` challenge (RFC 9728
/// §5.1): where the metadata is, and with no token, the scope to ask for first.
pub fn bearer_params(public_url: &str, with_scope: bool) -> String {
    let mut s = format!(", resource_metadata={}", quoted(&metadata_url(public_url)));
    if with_scope {
        s.push_str(&format!(", scope={}", quoted(READ)));
    }
    s
}

/// The metadata document of RFC 9728 §2 for the server at `public_url` whose
/// authorization server is `issuer`.
pub fn metadata(public_url: &str, issuer: &str) -> serde_json::Value {
    serde_json::json!({
        "resource": resource(public_url),
        "authorization_servers": [issuer],
        "scopes_supported": SUPPORTED,
        "bearer_methods_supported": ["header"],
        "resource_name": "Sparkles MCP server",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn scopes_map_to_a_bound() {
        let sc = scope_of(&s(&["sparkles:read"]));
        assert_eq!(sc.datasets, [("*".to_string(), Level::Read)].into());
        assert_eq!(sc.server, ["federate"]);
        let sc = scope_of(&s(&["sparkles:read", "sparkles:update:wiki"]));
        assert_eq!(sc.datasets["*"], Level::Read);
        assert_eq!(sc.datasets["wiki"], Level::Write);
        let sc = scope_of(&s(&["sparkles:admin"]));
        assert_eq!(sc.server, ["*"]);
        let sc = scope_of(&s(&["sparkles:admin:wiki"]));
        assert_eq!(sc.server, ["federate"]);
        assert_eq!(sc.datasets["wiki"], Level::Admin);
        // unknown or malformed scopes add nothing
        for bad in [
            "sparkles:write",
            "sparkles:read:",
            "sparkles:read:a/b",
            "sparkles:read:.x",
            "sparkles:",
            "openid",
        ] {
            let sc = scope_of(&s(&[bad]));
            assert!(sc.datasets.is_empty() && sc.server.is_empty(), "{bad}");
        }
        assert_eq!(
            sparkles_scopes(&s(&["openid", "sparkles:read", "x:sparkles:read"])),
            ["sparkles:read"]
        );
    }

    #[test]
    fn challenges_are_quoted() {
        let p = "https://sparql.example.org";
        assert_eq!(resource(p), "https://sparql.example.org/$/mcp");
        assert_eq!(
            insufficient_scope(p, UPDATE, "say \"hi\""),
            "Bearer error=\"insufficient_scope\", scope=\"sparkles:update\", \
             resource_metadata=\"https://sparql.example.org/.well-known/oauth-protected-resource/$/mcp\", \
             error_description=\"say \\\"hi\\\"\""
        );
        assert_eq!(
            bearer_params(p, true),
            ", resource_metadata=\"https://sparql.example.org/.well-known/oauth-protected-resource/$/mcp\", scope=\"sparkles:read\""
        );
    }
}
