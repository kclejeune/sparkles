//! PURL and w3id redirection recipe generator for hosted vocabulary namespaces.
//!
//! Generates Apache `.htaccess` rules (for `w3id.org` pull requests or self-hosted
//! reverse proxies) and PURL redirection recipes that forward canonical vocabulary IRIs
//! and version IRIs to Sparkles's `/$/ld` linked-data front end. Representation
//! negotiation (`Accept`, `q`-values, `Vary: Accept`) is delegated to Sparkles rather
//! than approximated with `RewriteCond` regexes.

use crate::resources::{Kind, Resource};
use serde::Serialize;
use std::fmt::Write as _;

/// Recipe format to generate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipeKind {
    /// Apache `.htaccess` suitable for w3id.org.
    W3id,
    /// Apache `.htaccess` for a self-hosted domain.
    Htaccess,
    /// PURL redirection recipe lines.
    Purl,
}

impl RecipeKind {
    pub fn parse(s: &str) -> Option<RecipeKind> {
        match s.trim().to_ascii_lowercase().as_str() {
            "w3id" => Some(RecipeKind::W3id),
            "htaccess" | "apache" => Some(RecipeKind::Htaccess),
            "purl" => Some(RecipeKind::Purl),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RecipeKind::W3id => "w3id",
            RecipeKind::Htaccess => "htaccess",
            RecipeKind::Purl => "purl",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NamespaceRecipe {
    pub prefix: String,
    pub iri: String,
    pub style: &'static str,
    pub target_url: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionRecipe {
    pub version: String,
    pub version_iri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<String>,
    pub target_url: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeExport {
    pub resource: String,
    pub kind: &'static str,
    pub base_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
    pub namespaces: Vec<NamespaceRecipe>,
    pub versions: Vec<VersionRecipe>,
    pub content: String,
}

fn has_rewrite_meta(s: &str) -> bool {
    s.chars().any(|c| {
        c.is_control()
            || c.is_whitespace()
            || matches!(c, '"' | '\'' | '\\' | '$' | '%' | '{' | '}' | '[' | ']')
    })
}

/// Validate a public server base URL (`http://` or `https://`, no whitespace, control
/// characters, credentials, query, fragment or Apache `mod_rewrite` metacharacters) and
/// return its normalized form without a trailing slash.
pub fn validate_base_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.len() > 1024 || has_rewrite_meta(trimmed) {
        return Err(format!("base: {raw:?} is not a valid http(s) URL"));
    }
    let u = url::Url::parse(trimmed)
        .map_err(|_| format!("base: {raw:?} is not a valid http(s) URL"))?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none_or(str::is_empty)
        || !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(format!(
            "base: {raw:?} must be an absolute http(s) URL without credentials, query or fragment"
        ));
    }
    let normalized = u.as_str().trim_end_matches('/').to_string();
    if has_rewrite_meta(&normalized) {
        return Err(format!("base: {raw:?} is not a valid http(s) URL"));
    }
    Ok(normalized)
}

/// Validate an explicit `.htaccess` directory mount (relative path without leading or
/// trailing slashes; empty string means site root `/`).
pub fn validate_mount(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.len() > 512
        || has_rewrite_meta(trimmed)
        || trimmed.contains('?')
        || trimmed.contains('#')
        || trimmed.split('/').any(|seg| seg == "..")
    {
        return Err(format!("mount: {raw:?} is not a valid directory path"));
    }
    Ok(trimmed.to_string())
}

fn escape_regex(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '.' | '+' | '*' | '?' | '^' | '$' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape literal characters in an Apache `mod_rewrite` `RewriteRule` substitution
/// string so `%N` (such as `%3A`, `%2F`), `$N` and `\` are emitted literally rather than
/// expanded as backreferences.
pub fn escape_rewrite_subst(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        if matches!(c, '\\' | '%' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Validate that a catalog namespace or version IRI can be represented by HTTP path-based
/// Apache / PURL redirection recipes without losing IRI identity (must be `http://` or
/// `https://` with a host, no query string, and no non-empty fragment).
fn validate_recipe_iri(iri: &str, label: &str) -> Result<(), String> {
    if iri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(format!("{label}: {iri:?} contains invalid characters"));
    }
    let u = url::Url::parse(iri)
        .map_err(|_| format!("{label}: {iri:?} is not a valid absolute http(s) IRI"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none_or(str::is_empty) {
        return Err(format!(
            "{label}: {iri:?} must use http:// or https:// for HTTP redirection recipes"
        ));
    }
    if u.query().is_some() {
        return Err(format!(
            "{label}: {iri:?} contains a query component, which path-based redirection recipes cannot preserve"
        ));
    }
    if u.fragment().is_some_and(|f| !f.is_empty()) {
        return Err(format!(
            "{label}: {iri:?} contains a non-empty fragment, which HTTP requests do not transmit"
        ));
    }
    Ok(())
}

fn percent_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Derive the shared top-level w3id directory mount (e.g. `myorg` for
/// `https://w3id.org/myorg/mv/` and `https://w3id.org/myorg/mvh#`) when all namespace
/// and version IRIs share the same valid first path segment.
fn derive_w3id_mount(namespaces: &[NamespaceRecipe], versions: &[VersionRecipe]) -> Option<String> {
    let mut shared: Option<String> = None;
    for iri in namespaces
        .iter()
        .map(|n| n.iri.as_str())
        .chain(versions.iter().map(|v| v.version_iri.as_str()))
    {
        let path = url::Url::parse(iri)
            .ok()
            .and_then(|u| u.path().strip_prefix('/').map(str::to_string))?;
        let first = path
            .split('/')
            .next()
            .filter(|s| !s.is_empty() && validate_mount(s).is_ok())?;
        match &shared {
            None => shared = Some(first.to_string()),
            Some(prev) if prev == first => {}
            Some(_) => return None,
        }
    }
    shared
}

/// Exact wire-level request-target path for `iri` matched against Apache's
/// `%{THE_REQUEST}`, preserving trailing and repeated slashes as well as percent-encoded
/// octets and scoping under `mount` when `iri`'s path does not already start with it.
fn wire_request_path(iri: &str, mount: Option<&str>) -> String {
    let raw = match url::Url::parse(iri) {
        Ok(u) => u.path().to_string(),
        Err(_) => format!("/{}", iri.trim_start_matches('/')),
    };
    let Some(m) = mount.filter(|s| !s.is_empty()) else {
        return raw;
    };
    let prefix = format!("/{m}");
    if raw == prefix || raw.starts_with(&format!("/{m}/")) {
        raw
    } else {
        format!("/{m}{raw}")
    }
}

/// Generate PURL / w3id redirection recipes for vocabulary resource `r`, using `mount`
/// when provided or deriving the default mount for `kind`.
pub fn generate(
    r: &Resource,
    kind: RecipeKind,
    base_url: &str,
    explicit_mount: Option<&str>,
) -> Result<RecipeExport, String> {
    if r.input.kind != Kind::Vocabulary {
        return Err(format!(
            "{} is a {}; only a vocabulary has hosted namespaces for recipes",
            r.id,
            r.input.kind.as_str()
        ));
    }
    let base = validate_base_url(base_url)?;
    let mut namespaces = Vec::new();
    for ns in &r.input.namespaces {
        validate_recipe_iri(&ns.iri, &format!("namespace '{}'", ns.prefix))?;
        let style = if ns.iri.ends_with('#') {
            "hash"
        } else {
            "slash"
        };
        let target_url = format!("{base}/$/ld/ns/{}", percent_encode(&ns.prefix));
        namespaces.push(NamespaceRecipe {
            prefix: ns.prefix.clone(),
            iri: ns.iri.clone(),
            style,
            target_url,
        });
    }

    let mut versions = Vec::new();
    for v in &r.versions {
        if let Some(vi) = &v.input.version_iri {
            validate_recipe_iri(vi, &format!("version '{}'", v.input.version))?;
            let target_url = format!("{base}/$/ld?iri={}", percent_encode(vi));
            versions.push(VersionRecipe {
                version: v.input.version.clone(),
                version_iri: vi.clone(),
                snapshot: v.input.snapshot.clone(),
                target_url,
            });
        }
    }

    let mount = match kind {
        RecipeKind::Purl => None,
        RecipeKind::W3id => match explicit_mount {
            Some(m) => {
                let v = validate_mount(m)?;
                (!v.is_empty()).then_some(v)
            }
            None => derive_w3id_mount(&namespaces, &versions),
        },
        RecipeKind::Htaccess => match explicit_mount {
            Some(m) => {
                let v = validate_mount(m)?;
                (!v.is_empty()).then_some(v)
            }
            None => None,
        },
    };

    let content = match kind {
        RecipeKind::W3id | RecipeKind::Htaccess => {
            render_htaccess(r, kind, &base, mount.as_deref(), &namespaces, &versions)
        }
        RecipeKind::Purl => render_purl(r, &base, &namespaces, &versions),
    };

    Ok(RecipeExport {
        resource: r.id.clone(),
        kind: kind.as_str(),
        base_url: base,
        mount,
        namespaces,
        versions,
        content,
    })
}

fn exact_request_pattern(wire_path: &str) -> String {
    format!(r"^\S+\s+{}(?:\s|$)", escape_regex(wire_path))
}

fn subterm_request_pattern(wire_path: &str) -> String {
    format!(r"^\S+\s+{}([^?\s]+)(?:\s|$)", escape_regex(wire_path))
}

fn subterm_query_request_pattern(wire_path: &str) -> String {
    format!(
        r"^\S+\s+{}([^?\s]*)\?([^?\s]*)(?:\s|$)",
        escape_regex(wire_path)
    )
}

fn subterm_multi_query_request_pattern(wire_path: &str) -> String {
    format!(
        r"^\S+\s+{}[^?\s]*\?\S*\?\S*(?:\s|$)",
        escape_regex(wire_path)
    )
}

fn write_dynamic_slash_rule(
    out: &mut String,
    tag: &str,
    wire_path: &str,
    subst_base: &str,
    subst_iri: &str,
) {
    let query_pat = subterm_query_request_pattern(wire_path);
    let multi_q_pat = subterm_multi_query_request_pattern(wire_path);
    let sub_pat = subterm_request_pattern(wire_path);

    // 1. Dynamic slash IRIs carrying a query component (`.../Term?edition=1` or
    //    `.../?edition=1`): encode the raw path suffix and raw query string in separate
    //    `[B]` passes so Apache neither drops nor URL-decodes either part.
    let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ^$");
    let _ = writeln!(out, "RewriteCond %{{THE_REQUEST}} {query_pat}");
    let _ = writeln!(
        out,
        "RewriteRule ^ __sparkles_ld_p__/%1 [DPI,QSD,E=SPARKLES_LD:{tag}q,E=SPARKLES_LD_Q:%2]"
    );
    let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ={tag}q");
    let _ = writeln!(
        out,
        "RewriteRule ^__sparkles_ld_p__/(.*)$ __sparkles_ld_q__/$1 [B,DPI,QSD,E=SPARKLES_LD:{tag}q2]"
    );
    let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ={tag}q2");
    let _ = writeln!(
        out,
        "RewriteRule ^__sparkles_ld_q__/(.*)$ __sparkles_ld_f__/%{{ENV:SPARKLES_LD_Q}} [DPI,QSD,E=SPARKLES_LD:{tag}q3,E=SPARKLES_LD_P:$1]"
    );
    let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ={tag}q3");
    let _ = writeln!(
        out,
        "RewriteRule ^__sparkles_ld_f__/(.*)$ {subst_base}/$/ld/doc?iri={subst_iri}%{{ENV:SPARKLES_LD_P}}\\%3F$1 [B,QSD,R=303,L,NE]"
    );

    // 2. Explicitly reject requests whose query contains an additional literal `?`
    //    rather than silently altering or dropping the query suffix.
    let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ^$");
    let _ = writeln!(out, "RewriteCond %{{THE_REQUEST}} {multi_q_pat}");
    let _ = writeln!(out, "RewriteRule ^ - [R=400,L]");

    // 3. Dynamic slash IRIs without a query string: capture the raw (un-decoded) suffix
    //    from %{THE_REQUEST} into an internal marker path with [DPI], then let [B]
    //    percent-encode it for the `?iri=` parameter while keeping RDF IRI identity exact.
    let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ^$");
    let _ = writeln!(out, "RewriteCond %{{THE_REQUEST}} {sub_pat}");
    let _ = writeln!(
        out,
        "RewriteRule ^ __sparkles_ld__/%1 [DPI,QSD,E=SPARKLES_LD:{tag}]"
    );
    let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ={tag}");
    let _ = writeln!(
        out,
        "RewriteRule ^__sparkles_ld__/(.+)$ {subst_base}/$/ld/doc?iri={subst_iri}$1 [B,QSD,R=303,L,NE]"
    );
}

fn render_htaccess(
    r: &Resource,
    kind: RecipeKind,
    base: &str,
    mount: Option<&str>,
    namespaces: &[NamespaceRecipe],
    versions: &[VersionRecipe],
) -> String {
    let mut out = String::new();
    let header_kind = match kind {
        RecipeKind::W3id => "w3id.org Apache .htaccess",
        RecipeKind::Htaccess => "Apache .htaccess",
        RecipeKind::Purl => unreachable!(),
    };
    let _ = writeln!(out, "# {header_kind} recipe for vocabulary '{}'", r.id);
    let _ = writeln!(out, "# Generated by Sparkles linked-data front end");
    let _ = writeln!(out, "# Target server: {base}");
    match (kind, mount) {
        (RecipeKind::W3id, Some(m)) => {
            let _ = writeln!(
                out,
                "# Mount directory: /{m}/ (place this file at ids/{m}/.htaccess)"
            );
        }
        (RecipeKind::W3id, None) => {
            let _ = writeln!(
                out,
                "# Mount directory: / (site root; pass ?mount=<dir> for a per-identifier directory)"
            );
        }
        (RecipeKind::Htaccess, Some(m)) => {
            let _ = writeln!(
                out,
                "# Mount directory: /{m}/ (place this file at {m}/.htaccess)"
            );
        }
        (RecipeKind::Htaccess, None) => {
            let _ = writeln!(
                out,
                "# Mount directory: / (document root .htaccess; pass ?mount=<dir> for a subdirectory)"
            );
        }
        (RecipeKind::Purl, _) => unreachable!(),
    }
    let _ = writeln!(
        out,
        "# Content negotiation (Accept, q-values, Vary: Accept) is handled by Sparkles"
    );
    let _ = writeln!(
        out,
        "# Note: terms with encoded path separators (%2F) require `AllowEncodedSlashes NoDecode`"
    );
    let _ = writeln!(
        out,
        "# in the enclosing Apache VirtualHost (Apache forbids it inside .htaccess)."
    );
    let _ = writeln!(out, "Options +FollowSymLinks");
    let _ = writeln!(out, "RewriteEngine on");
    let _ = writeln!(out);

    let subst_base = escape_rewrite_subst(base);

    // Version IRI rules come before general namespace rules, ordered longest-first so
    // more specific prefixes (including repeated-slash prefixes) match before parents.
    if !versions.is_empty() {
        let _ = writeln!(out, "# Versioned IRI rules");
        let mut ver_order: Vec<usize> = (0..versions.len()).collect();
        ver_order.sort_by(|&a, &b| {
            versions[b]
                .version_iri
                .len()
                .cmp(&versions[a].version_iri.len())
                .then_with(|| a.cmp(&b))
        });
        for i in ver_order {
            let v = &versions[i];
            let wire_path = wire_request_path(&v.version_iri, mount);
            let subst_vi = escape_rewrite_subst(&percent_encode(&v.version_iri));
            let _ = writeln!(
                out,
                "# Version {} ({}){}",
                v.version,
                v.version_iri,
                v.snapshot
                    .as_deref()
                    .map(|s| format!(" -> {s}"))
                    .unwrap_or_default()
            );
            let exact_pat = exact_request_pattern(&wire_path);
            let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ^$");
            let _ = writeln!(out, "RewriteCond %{{THE_REQUEST}} {exact_pat}");
            let _ = writeln!(
                out,
                "RewriteRule ^ {subst_base}/$/ld/doc?iri={subst_vi} [QSD,R=303,L,NE]"
            );
            if v.version_iri.ends_with('/') || v.version_iri.ends_with(':') {
                write_dynamic_slash_rule(
                    &mut out,
                    &format!("v{i}"),
                    &wire_path,
                    &subst_base,
                    &subst_vi,
                );
            }
            let _ = writeln!(out);
        }
    }

    let mut ns_order: Vec<usize> = (0..namespaces.len()).collect();
    ns_order.sort_by(|&a, &b| {
        namespaces[b]
            .iri
            .len()
            .cmp(&namespaces[a].iri.len())
            .then_with(|| a.cmp(&b))
    });
    for i in ns_order {
        let ns = &namespaces[i];
        let wire_path = wire_request_path(&ns.iri, mount);
        let subst_prefix = escape_rewrite_subst(&percent_encode(&ns.prefix));
        let subst_ns = escape_rewrite_subst(&percent_encode(&ns.iri));
        let _ = writeln!(
            out,
            "# Namespace '{}' ({}, {} IRI)",
            ns.prefix, ns.iri, ns.style
        );
        let root_pat = exact_request_pattern(&wire_path);
        if ns.style == "hash" {
            let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ^$");
            let _ = writeln!(out, "RewriteCond %{{THE_REQUEST}} {root_pat}");
            let _ = writeln!(
                out,
                "RewriteRule ^ {subst_base}/$/ld/ns/{subst_prefix} [QSD,R=303,L,NE]"
            );
        } else {
            if ns.iri.ends_with('/') || ns.iri.ends_with(':') {
                write_dynamic_slash_rule(
                    &mut out,
                    &format!("n{i}"),
                    &wire_path,
                    &subst_base,
                    &subst_ns,
                );
            }
            let _ = writeln!(out, "RewriteCond %{{ENV:SPARKLES_LD}} ^$");
            let _ = writeln!(out, "RewriteCond %{{THE_REQUEST}} {root_pat}");
            let _ = writeln!(
                out,
                "RewriteRule ^ {subst_base}/$/ld/doc?iri={subst_ns} [QSD,R=303,L,NE]"
            );
        }
        let _ = writeln!(out);
    }

    out
}

fn render_purl(
    r: &Resource,
    base: &str,
    namespaces: &[NamespaceRecipe],
    versions: &[VersionRecipe],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# PURL redirection recipe for vocabulary '{}'", r.id);
    let _ = writeln!(out, "# Target server: {base}");
    let _ = writeln!(out);
    for v in versions {
        let source = v.version_iri.trim_end_matches('#');
        let _ = writeln!(
            out,
            "303 {} -> {}{}",
            source,
            v.target_url,
            v.snapshot
                .as_deref()
                .map(|s| format!(" # version {} ({s})", v.version))
                .unwrap_or_else(|| format!(" # version {}", v.version))
        );
    }
    for ns in namespaces {
        if ns.style == "hash" {
            let _ = writeln!(
                out,
                "303 {} -> {} # hash namespace '{}'",
                ns.iri.trim_end_matches('#'),
                ns.target_url,
                ns.prefix
            );
        } else {
            let _ = writeln!(
                out,
                "partial {} -> {}/ # slash namespace '{}' (PURL partial sends 302; Sparkles serves both /$/ld/ns/{}/ and /$/ld/ns/{}/<term> with 303 See Other to /$/ld/doc)",
                ns.iri, ns.target_url, ns.prefix, ns.prefix, ns.prefix
            );
        }
    }
    out
}
