//! Safe, escaped HTML5 renderer for linked-data resource descriptions.
//!
//! All dynamic text, IRIs, language tags, datatypes and attribute values are strictly
//! HTML-escaped. Only `http://`, `https://` and relative `/$/ld...` URLs are rendered
//! as clickable `href` links; any other IRI scheme (such as `javascript:` or `data:`)
//! is rendered as plain escaped text.

use super::{Description, Format};
use oxrdf::{NamedOrBlankNode, Term, Triple};
use std::collections::BTreeMap;
use std::fmt::Write as _;

const RDFS_LABEL: &str = "http://www.w3.org/2000/01/rdf-schema#label";
const SKOS_PREF_LABEL: &str = "http://www.w3.org/2004/02/skos/core#prefLabel";
const DCT_TITLE: &str = "http://purl.org/dc/terms/title";
const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";

/// Strict CSP for linked-data HTML pages: no scripts, no external subresources, no framing.
pub const HTML_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'";

/// Escape `&`, `<`, `>`, `"`, and `'` for safe HTML text and quoted attribute contexts.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// Whether `iri` may be placed in an HTML `href` attribute directly.
/// Only absolute `http://` and `https://` URLs without whitespace or control chars pass.
pub fn safe_external_href(iri: &str) -> bool {
    if iri.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let lower = iri.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

fn percent_encode_param(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Build a `/$/ld/doc` link for `iri` and `format`, preserving optional `version` and `at`.
pub fn doc_url(
    iri: &str,
    format: Format,
    version: Option<&str>,
    at: Option<&str>,
    extra_describe: &[(String, String)],
) -> String {
    let mut url = format!(
        "/$/ld/doc?iri={}&format={}",
        percent_encode_param(iri),
        format.param_name()
    );
    if let Some(v) = version {
        let _ = write!(url, "&version={}", percent_encode_param(v));
    }
    if let Some(a) = at {
        let _ = write!(url, "&at={}", percent_encode_param(a));
    }
    for (k, v) in extra_describe {
        let _ = write!(
            url,
            "&{}={}",
            percent_encode_param(k),
            percent_encode_param(v)
        );
    }
    url
}

fn compact_iri(iri: &str, prefixes: &BTreeMap<String, String>) -> Option<String> {
    let mut best: Option<(&str, &str)> = None;
    for (prefix, ns) in prefixes {
        if let Some(local) = iri.strip_prefix(ns.as_str())
            && !local.is_empty()
            && !local.contains('/')
            && best.is_none_or(|(_, b_ns)| ns.len() > b_ns.len())
        {
            best = Some((prefix.as_str(), ns.as_str()));
        }
    }
    best.map(|(p, ns)| format!("{p}:{}", &iri[ns.len()..]))
}

fn is_readable_hosted(iri: &str, readable_namespaces: &[String]) -> bool {
    readable_namespaces.iter().any(|ns| {
        iri == ns.as_str()
            || ((ns.ends_with('/') || ns.ends_with('#') || ns.ends_with(':'))
                && iri.starts_with(ns.as_str()))
    })
}

fn render_iri(
    iri: &str,
    prefixes: &BTreeMap<String, String>,
    readable_namespaces: &[String],
    version: Option<&str>,
    at: Option<&str>,
) -> String {
    let display = compact_iri(iri, prefixes).unwrap_or_else(|| iri.to_string());
    let escaped_display = escape_html(&display);
    let escaped_full = escape_html(iri);
    if is_readable_hosted(iri, readable_namespaces) {
        let mut href = format!("/$/ld?iri={}", percent_encode_param(iri));
        if let Some(v) = version {
            let _ = write!(href, "&version={}", percent_encode_param(v));
        }
        if let Some(a) = at {
            let _ = write!(href, "&at={}", percent_encode_param(a));
        }
        format!(
            "<a href=\"{}\" title=\"{escaped_full}\">{escaped_display}</a>",
            escape_html(&href)
        )
    } else if safe_external_href(iri) {
        format!(
            "<a href=\"{escaped_full}\" rel=\"noopener noreferrer\" title=\"{escaped_full}\">{escaped_display}</a>"
        )
    } else {
        format!("<code title=\"{escaped_full}\">{escaped_display}</code>")
    }
}

fn render_term(
    term: &Term,
    prefixes: &BTreeMap<String, String>,
    readable_namespaces: &[String],
    version: Option<&str>,
    at: Option<&str>,
) -> String {
    match term {
        Term::NamedNode(n) => render_iri(n.as_str(), prefixes, readable_namespaces, version, at),
        Term::BlankNode(b) => format!("<code>_:{}</code>", escape_html(b.as_str())),
        Term::Literal(l) => {
            let val = escape_html(l.value());
            if let Some(lang) = l.language() {
                format!(
                    "<span class=\"lit\">{val}</span> <span class=\"tag\">@{}</span>",
                    escape_html(lang)
                )
            } else {
                let dt = l.datatype().as_str();
                if dt == "http://www.w3.org/2001/XMLSchema#string" {
                    format!("<span class=\"lit\">{val}</span>")
                } else {
                    let dt_disp = compact_iri(dt, prefixes).unwrap_or_else(|| dt.to_string());
                    format!(
                        "<span class=\"lit\">{val}</span> <span class=\"tag\">^^{}</span>",
                        escape_html(&dt_disp)
                    )
                }
            }
        }
        #[allow(unreachable_patterns)]
        other => format!("<code>{}</code>", escape_html(&other.to_string())),
    }
}

fn preferred_label(iri: &str, triples: &[Triple]) -> String {
    let mut best: Option<(u8, String)> = None;
    for t in triples {
        let NamedOrBlankNode::NamedNode(s) = &t.subject else {
            continue;
        };
        if s.as_str() != iri {
            continue;
        }
        let pred = t.predicate.as_str();
        let pred_rank = match pred {
            SKOS_PREF_LABEL => 0u8,
            RDFS_LABEL => 1u8,
            DCT_TITLE => 2u8,
            _ => continue,
        };
        let Term::Literal(lit) = &t.object else {
            continue;
        };
        let lang_rank = match lit.language() {
            Some(l) if l.eq_ignore_ascii_case("en") => 0u8,
            None => 1u8,
            Some(_) => 2u8,
        };
        let rank = pred_rank * 4 + lang_rank;
        if best.as_ref().is_none_or(|(b, _)| rank < *b) {
            best = Some((rank, lit.value().to_string()));
        }
    }
    if let Some((_, label)) = best {
        return label;
    }
    // Fall back to local name if present, else the full IRI.
    iri.rsplit_once('#')
        .or_else(|| iri.trim_end_matches('/').rsplit_once('/'))
        .map(|(_, local)| local)
        .filter(|s| !s.is_empty())
        .unwrap_or(iri)
        .to_string()
}

/// Render a self-contained, escaped HTML5 page for `desc`.
pub fn render(
    desc: &Description,
    version_param: Option<&str>,
    at_param: Option<&str>,
    extra_describe: &[(String, String)],
) -> String {
    let title = preferred_label(&desc.iri, &desc.triples);
    let mut outgoing: Vec<&Triple> = Vec::new();
    let mut incoming: Vec<&Triple> = Vec::new();
    let mut related: Vec<&Triple> = Vec::new();

    for t in &desc.triples {
        let is_subj =
            matches!(&t.subject, NamedOrBlankNode::NamedNode(n) if n.as_str() == desc.iri);
        let is_obj = matches!(&t.object, Term::NamedNode(n) if n.as_str() == desc.iri);
        if is_subj {
            outgoing.push(t);
        } else if is_obj {
            incoming.push(t);
        } else {
            related.push(t);
        }
    }
    // Sort deterministically: rdf:type first, then predicate, then object.
    outgoing.sort_by(|a, b| {
        let ra = a.predicate.as_str() != RDF_TYPE;
        let rb = b.predicate.as_str() != RDF_TYPE;
        (ra, a.predicate.as_str(), a.object.to_string()).cmp(&(
            rb,
            b.predicate.as_str(),
            b.object.to_string(),
        ))
    });
    incoming.sort_by(|a, b| {
        (a.predicate.as_str(), a.subject.to_string())
            .cmp(&(b.predicate.as_str(), b.subject.to_string()))
    });
    related.sort_by(|a, b| {
        (
            a.subject.to_string(),
            a.predicate.as_str(),
            a.object.to_string(),
        )
            .cmp(&(
                b.subject.to_string(),
                b.predicate.as_str(),
                b.object.to_string(),
            ))
    });

    let mut out = String::with_capacity(4096);
    let escaped_title = escape_html(&title);
    let escaped_iri = escape_html(&desc.iri);
    let _ = write!(
        out,
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{escaped_title}</title>\n"
    );
    for fmt in [
        Format::Turtle,
        Format::JsonLd,
        Format::RdfXml,
        Format::NTriples,
    ] {
        let href = escape_html(&doc_url(
            &desc.iri,
            fmt,
            version_param,
            at_param,
            extra_describe,
        ));
        let _ = writeln!(
            out,
            "<link rel=\"alternate\" type=\"{}\" href=\"{href}\" title=\"{}\">",
            fmt.essence(),
            fmt.label()
        );
    }
    out.push_str(
        "<style>\n\
         body{font-family:system-ui,-apple-system,sans-serif;max-width:60rem;margin:2rem auto;padding:0 1rem;line-height:1.5;color:#1f2937;background:#fff}\n\
         h1{margin:0 0 .25rem;font-size:1.6rem}\n\
         .iri{font-family:ui-monospace,SFMono-Regular,monospace;font-size:.9rem;color:#4b5563;word-break:break-all;margin-bottom:1rem}\n\
         .meta{display:flex;flex-wrap:wrap;gap:.5rem 1.25rem;font-size:.875rem;color:#374151;background:#f9fafb;border:1px solid #e5e7eb;border-radius:6px;padding:.75rem 1rem;margin-bottom:1.25rem}\n\
         .formats{display:flex;flex-wrap:wrap;gap:.5rem;margin-bottom:1.5rem;font-size:.875rem}\n\
         .formats a{padding:.25rem .6rem;border:1px solid #d1d5db;border-radius:4px;text-decoration:none;color:#1d4ed8;background:#f8fafc}\n\
         .notice{background:#fef3c7;border:1px solid #f59e0b;border-radius:6px;padding:.6rem .9rem;margin-bottom:1.25rem;font-size:.875rem}\n\
         h2{font-size:1.1rem;margin:1.5rem 0 .5rem;border-bottom:1px solid #e5e7eb;padding-bottom:.25rem}\n\
         table{width:100%;border-collapse:collapse;font-size:.92rem}\n\
         th,td{text-align:left;vertical-align:top;padding:.45rem .6rem;border-bottom:1px solid #f3f4f6}\n\
         th{width:28%;font-weight:600;color:#374151}\n\
         code{font-family:ui-monospace,SFMono-Regular,monospace;font-size:.88em;word-break:break-all}\n\
         .tag{font-family:ui-monospace,SFMono-Regular,monospace;font-size:.78em;color:#6b7280}\n\
         a{color:#1d4ed8}\n\
         </style>\n\
         </head>\n\
         <body>\n",
    );
    let _ = writeln!(out, "<h1>{escaped_title}</h1>");
    let _ = writeln!(out, "<div class=\"iri\"><code>{escaped_iri}</code></div>");

    // Metadata bar
    out.push_str("<div class=\"meta\">");
    let _ = write!(
        out,
        "<span><strong>Resource:</strong> {}</span>",
        escape_html(&desc.resource_title)
    );
    if let Some(p) = &desc.namespace_prefix {
        let _ = write!(
            out,
            "<span><strong>Prefix:</strong> <code>{}</code></span>",
            escape_html(p)
        );
    }
    if let Some(v) = &desc.version_name {
        let _ = write!(
            out,
            "<span><strong>Version:</strong> <code>{}</code></span>",
            escape_html(v)
        );
    }
    if let Some(h) = &desc.resolved_history {
        let _ = write!(
            out,
            "<span><strong>Snapshot:</strong> <code>{}</code> (commit {})</span>",
            escape_html(&h.at.to_string()),
            h.commit.seq
        );
    }
    if let Some(lic) = &desc.license {
        let _ = write!(
            out,
            "<span><strong>Licence:</strong> <code>{}</code></span>",
            escape_html(lic)
        );
    }
    out.push_str("</div>\n");

    // Alternate RDF format links
    out.push_str("<nav class=\"formats\" aria-label=\"Alternate formats\">");
    for fmt in [
        Format::Turtle,
        Format::JsonLd,
        Format::RdfXml,
        Format::NTriples,
    ] {
        let href = escape_html(&doc_url(
            &desc.iri,
            fmt,
            version_param,
            at_param,
            extra_describe,
        ));
        let _ = write!(out, "<a href=\"{href}\">{}</a>", fmt.label());
    }
    out.push_str("</nav>\n");

    if desc.truncated {
        out.push_str(
            "<div class=\"notice\">This description was truncated at the server's DESCRIBE triple limit.</div>\n",
        );
    }

    if !outgoing.is_empty() {
        out.push_str("<h2>Properties</h2>\n<table><tbody>\n");
        for t in outgoing {
            let p_html = render_iri(
                t.predicate.as_str(),
                &desc.prefixes,
                &desc.readable_namespaces,
                version_param,
                at_param,
            );
            let o_html = render_term(
                &t.object,
                &desc.prefixes,
                &desc.readable_namespaces,
                version_param,
                at_param,
            );
            let _ = writeln!(out, "<tr><th>{p_html}</th><td>{o_html}</td></tr>");
        }
        out.push_str("</tbody></table>\n");
    }

    if !incoming.is_empty() {
        out.push_str("<h2>Referenced by</h2>\n<table><tbody>\n");
        for t in incoming {
            let s_html = match &t.subject {
                NamedOrBlankNode::NamedNode(n) => render_iri(
                    n.as_str(),
                    &desc.prefixes,
                    &desc.readable_namespaces,
                    version_param,
                    at_param,
                ),
                NamedOrBlankNode::BlankNode(b) => {
                    format!("<code>_:{}</code>", escape_html(b.as_str()))
                }
            };
            let p_html = render_iri(
                t.predicate.as_str(),
                &desc.prefixes,
                &desc.readable_namespaces,
                version_param,
                at_param,
            );
            let _ = writeln!(out, "<tr><th>{p_html}</th><td>{s_html}</td></tr>");
        }
        out.push_str("</tbody></table>\n");
    }

    if !related.is_empty() {
        out.push_str("<h2>Related statements</h2>\n<table><thead><tr><th>Subject</th><th>Predicate</th><th>Object</th></tr></thead><tbody>\n");
        for t in related {
            let s_html = match &t.subject {
                NamedOrBlankNode::NamedNode(n) => render_iri(
                    n.as_str(),
                    &desc.prefixes,
                    &desc.readable_namespaces,
                    version_param,
                    at_param,
                ),
                NamedOrBlankNode::BlankNode(b) => {
                    format!("<code>_:{}</code>", escape_html(b.as_str()))
                }
            };
            let p_html = render_iri(
                t.predicate.as_str(),
                &desc.prefixes,
                &desc.readable_namespaces,
                version_param,
                at_param,
            );
            let o_html = render_term(
                &t.object,
                &desc.prefixes,
                &desc.readable_namespaces,
                version_param,
                at_param,
            );
            let _ = writeln!(
                out,
                "<tr><td>{s_html}</td><td>{p_html}</td><td>{o_html}</td></tr>"
            );
        }
        out.push_str("</tbody></table>\n");
    }

    out.push_str("</body>\n</html>\n");
    out
}
