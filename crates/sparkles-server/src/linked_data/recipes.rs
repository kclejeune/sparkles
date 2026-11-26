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

fn relative_path(iri: &str) -> String {
    match url::Url::parse(iri) {
        Ok(u) => u.path().trim_start_matches('/').to_string(),
        Err(_) => iri.trim_start_matches('/').to_string(),
    }
}

fn percent_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Derive the shared top-level w3id directory mount (e.g. `myorg` for
/// `https://w3id.org/myorg/mv/` and `https://w3id.org/myorg/mvh#`) when all namespace
/// and version IRIs share the same first path segment.
fn derive_w3id_mount(namespaces: &[NamespaceRecipe], versions: &[VersionRecipe]) -> Option<String> {
    let mut shared: Option<String> = None;
    for iri in namespaces
        .iter()
        .map(|n| n.iri.as_str())
        .chain(versions.iter().map(|v| v.version_iri.as_str()))
    {
        let path = relative_path(iri);
        let first = path
            .trim_end_matches('/')
            .split('/')
            .next()
            .filter(|s| !s.is_empty())?;
        match &shared {
            None => shared = Some(first.to_string()),
            Some(prev) if prev == first => {}
            Some(_) => return None,
        }
    }
    shared
}

/// Strip `mount` directory prefix from a URL's root-relative path so `.htaccess`
/// `RewriteRule` patterns match Apache's per-directory relative path.
fn strip_mount(path: &str, mount: Option<&str>) -> String {
    let p = path.trim_start_matches('/');
    let Some(m) = mount.filter(|s| !s.is_empty()) else {
        return p.to_string();
    };
    if p == m {
        String::new()
    } else if let Some(rest) = p.strip_prefix(m).and_then(|r| r.strip_prefix('/')) {
        rest.to_string()
    } else {
        p.to_string()
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

fn exact_pattern(pat: &str) -> String {
    if pat.is_empty() {
        "^$".to_string()
    } else {
        format!("^{pat}/?$")
    }
}

fn subterm_pattern(pat: &str) -> String {
    if pat.is_empty() {
        "^(.+)$".to_string()
    } else {
        format!("^{pat}/(.+)$")
    }
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
    let _ = writeln!(out, "Options +FollowSymLinks");
    let _ = writeln!(out, "RewriteEngine on");
    let _ = writeln!(out);

    let subst_base = escape_rewrite_subst(base);

    // Version IRI rules come before general namespace rules so specific version IRIs match first.
    if !versions.is_empty() {
        let _ = writeln!(out, "# Versioned IRI rules");
        for v in versions {
            let path = strip_mount(&relative_path(&v.version_iri), mount);
            let pat = escape_regex(path.trim_end_matches('/'));
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
            let exact_rule = exact_pattern(&pat);
            let _ = writeln!(
                out,
                "RewriteRule {exact_rule} {subst_base}/$/ld/doc?iri={subst_vi} [R=303,L,NE]"
            );
            if v.version_iri.ends_with('/') {
                let term_rule = subterm_pattern(&pat);
                let _ = writeln!(
                    out,
                    "RewriteRule {term_rule} {subst_base}/$/ld/doc?iri={subst_vi}$1 [B,R=303,L,NE]"
                );
            }
            let _ = writeln!(out);
        }
    }

    for ns in namespaces {
        let path = strip_mount(&relative_path(&ns.iri), mount);
        let pat = escape_regex(path.trim_end_matches('/'));
        let subst_prefix = escape_rewrite_subst(&percent_encode(&ns.prefix));
        let subst_ns = escape_rewrite_subst(&percent_encode(&ns.iri));
        let _ = writeln!(
            out,
            "# Namespace '{}' ({}, {} IRI)",
            ns.prefix, ns.iri, ns.style
        );
        let root_rule = exact_pattern(&pat);
        if ns.style == "hash" {
            let _ = writeln!(
                out,
                "RewriteRule {root_rule} {subst_base}/$/ld/ns/{subst_prefix} [R=303,L,NE]"
            );
        } else {
            let term_rule = subterm_pattern(&pat);
            let _ = writeln!(
                out,
                "RewriteRule {term_rule} {subst_base}/$/ld/doc?iri={subst_ns}$1 [B,R=303,L,NE]"
            );
            let _ = writeln!(
                out,
                "RewriteRule {root_rule} {subst_base}/$/ld/doc?iri={subst_ns} [R=303,L,NE]"
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
        let _ = writeln!(
            out,
            "303 {} -> {}{}",
            v.version_iri,
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
                "partial {} -> {}/ # slash namespace '{}' (PURL partial sends 302; Sparkles applies 303 See Other to /$/ld/doc)",
                ns.iri, ns.target_url, ns.prefix
            );
        }
    }
    out
}
