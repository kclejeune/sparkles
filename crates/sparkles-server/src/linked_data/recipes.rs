//! PURL and w3id redirection recipe generator for hosted vocabulary namespaces.
//!
//! Generates Apache `.htaccess` rules (for `w3id.org` pull requests or self-hosted
//! reverse proxies) and PURL redirection recipes that forward canonical vocabulary IRIs
//! and version IRIs to Sparkles's `/$/ld` linked-data front end with content negotiation.

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
    pub namespaces: Vec<NamespaceRecipe>,
    pub versions: Vec<VersionRecipe>,
    pub content: String,
}

/// Validate a public server base URL (`http://` or `https://`, no whitespace or control
/// characters, no query or fragment) and return it without a trailing slash.
pub fn validate_base_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > 1024
        || trimmed
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '"' | '\'' | '\\'))
    {
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
    Ok(trimmed.trim_end_matches('/').to_string())
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

fn relative_path(iri: &str) -> String {
    match url::Url::parse(iri) {
        Ok(u) => u.path().trim_start_matches('/').to_string(),
        Err(_) => iri.trim_start_matches('/').to_string(),
    }
}

fn percent_encode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Generate PURL / w3id redirection recipes for vocabulary resource `r`.
pub fn generate(r: &Resource, kind: RecipeKind, base_url: &str) -> Result<RecipeExport, String> {
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

    let content = match kind {
        RecipeKind::W3id | RecipeKind::Htaccess => {
            render_htaccess(r, kind, &base, &namespaces, &versions)
        }
        RecipeKind::Purl => render_purl(r, &base, &namespaces, &versions),
    };

    Ok(RecipeExport {
        resource: r.id.clone(),
        kind: kind.as_str(),
        base_url: base,
        namespaces,
        versions,
        content,
    })
}

fn render_htaccess(
    r: &Resource,
    kind: RecipeKind,
    base: &str,
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
    let _ = writeln!(out, "Options +FollowSymLinks");
    let _ = writeln!(out, "RewriteEngine on");
    let _ = writeln!(out);

    // Version IRI rules come before general namespace rules so specific version IRIs match first.
    if !versions.is_empty() {
        let _ = writeln!(out, "# Versioned IRI rules");
        for v in versions {
            let path = relative_path(&v.version_iri);
            let pat = escape_regex(path.trim_end_matches('/'));
            let enc_vi = percent_encode(&v.version_iri);
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
            for (accept, fmt) in [
                ("text/html", "html"),
                ("text/turtle", "turtle"),
                ("application/ld\\+json", "jsonld"),
                ("application/rdf\\+xml", "rdfxml"),
                ("application/n-triples", "ntriples"),
            ] {
                let _ = writeln!(out, "RewriteCond %{{HTTP_ACCEPT}} {accept} [NC]");
                let _ = writeln!(
                    out,
                    "RewriteRule ^{pat}/?$ {base}/$/ld/doc?iri={enc_vi}&format={fmt} [R=303,L]"
                );
            }
            let _ = writeln!(
                out,
                "RewriteRule ^{pat}/?$ {base}/$/ld/doc?iri={enc_vi}&format=turtle [R=303,L]"
            );
            if v.version_iri.ends_with('/') {
                let _ = writeln!(
                    out,
                    "RewriteRule ^{pat}/(.+)$ {base}/$/ld?iri={enc_vi}$1 [R=303,L,NE]"
                );
            }
            let _ = writeln!(out);
        }
    }

    for ns in namespaces {
        let path = relative_path(&ns.iri);
        let pat = escape_regex(path.trim_end_matches('/'));
        let enc_prefix = percent_encode(&ns.prefix);
        let enc_ns = percent_encode(&ns.iri);
        let _ = writeln!(
            out,
            "# Namespace '{}' ({}, {} IRI)",
            ns.prefix, ns.iri, ns.style
        );
        if ns.style == "hash" {
            for (accept, fmt) in [
                ("text/html", "html"),
                ("text/turtle", "turtle"),
                ("application/ld\\+json", "jsonld"),
                ("application/rdf\\+xml", "rdfxml"),
                ("application/n-triples", "ntriples"),
            ] {
                let _ = writeln!(out, "RewriteCond %{{HTTP_ACCEPT}} {accept} [NC]");
                let _ = writeln!(
                    out,
                    "RewriteRule ^{pat}/?$ {base}/$/ld/doc?iri={enc_ns}&format={fmt} [R=303,L]"
                );
            }
            let _ = writeln!(
                out,
                "RewriteRule ^{pat}/?$ {base}/$/ld/ns/{enc_prefix} [R=303,L]"
            );
        } else {
            let root_rule = if pat.is_empty() {
                "^$".to_string()
            } else {
                format!("^{pat}/?$")
            };
            let term_rule = if pat.is_empty() {
                "^(.+)$".to_string()
            } else {
                format!("^{pat}/(.+)$")
            };
            for (accept, fmt) in [
                ("text/html", "html"),
                ("text/turtle", "turtle"),
                ("application/ld\\+json", "jsonld"),
                ("application/rdf\\+xml", "rdfxml"),
                ("application/n-triples", "ntriples"),
            ] {
                let _ = writeln!(out, "RewriteCond %{{HTTP_ACCEPT}} {accept} [NC]");
                let _ = writeln!(
                    out,
                    "RewriteRule {term_rule} {base}/$/ld/doc?iri={enc_ns}$1&format={fmt} [R=303,L,NE]"
                );
            }
            let _ = writeln!(
                out,
                "RewriteRule {term_rule} {base}/$/ld/ns/{enc_prefix}/$1 [R=303,L,NE]"
            );
            let _ = writeln!(
                out,
                "RewriteRule {root_rule} {base}/$/ld/ns/{enc_prefix} [R=303,L]"
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
                "303 partial {} -> {}/ # slash namespace '{}'",
                ns.iri, ns.target_url, ns.prefix
            );
        }
    }
    out
}
