//! IRI and language-tag checks: `sparkles iri`, `sparkles langtag`, the warnings of
//! `convert --check` and `load --check`, and the scheme warnings of `/$/validate/iri`
//! (spec G05 §4).
//!
//! Errors are violations of the RFC 3987 grammar (oxiri) and of BCP 47's well-formedness
//! (oxilangtag). Warnings are syntax-based normalization issues of RFC 3986 §6 and the
//! rules of a few schemes: `http`, `https`, `urn` (with `uuid` and `oid`), `file` and
//! `did`.

use oxrdf::{GraphNameRef, NamedOrBlankNodeRef, QuadRef, TermRef};
use serde_json::{Value as J, json};
use std::collections::HashSet;

/// One problem with an IRI or a language tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    /// A short stable code, such as `scheme-case` (spec G05 §4).
    pub code: &'static str,
    pub message: String,
    /// An error: the value is not an IRI or a well-formed tag at all.
    pub error: bool,
}

impl Issue {
    fn warn(code: &'static str, message: impl Into<String>) -> Issue {
        Issue {
            code,
            message: message.into(),
            error: false,
        }
    }
    fn error(code: &'static str, message: impl Into<String>) -> Issue {
        Issue {
            code,
            message: message.into(),
            error: true,
        }
    }
    fn json(&self) -> J {
        json!({ "code": self.code, "message": self.message, "error": self.error })
    }
}

// ------------------------------------------------------------------------- IRIs ----

/// The components of an IRI reference, split at the delimiters of RFC 3986 §3 without
/// validation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Parts<'a> {
    pub scheme: Option<&'a str>,
    pub authority: Option<&'a str>,
    pub path: &'a str,
    pub query: Option<&'a str>,
    pub fragment: Option<&'a str>,
}

impl<'a> Parts<'a> {
    pub fn split(s: &'a str) -> Parts<'a> {
        let (rest, fragment) = match s.split_once('#') {
            Some((a, f)) => (a, Some(f)),
            None => (s, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        // a scheme is letters, digits, `+`, `-` and `.` before the first `:`, starting
        // with a letter
        let (scheme, rest) = match rest.find(':') {
            Some(i)
                if i > 0
                    && rest[..i].starts_with(|c: char| c.is_ascii_alphabetic())
                    && rest[..i]
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
                    && !rest[..i].contains('/') =>
            {
                (Some(&rest[..i]), &rest[i + 1..])
            }
            _ => (None, rest),
        };
        let (authority, path) = match rest.strip_prefix("//") {
            Some(r) => {
                let end = r.find('/').unwrap_or(r.len());
                (Some(&r[..end]), &r[end..])
            }
            None => (None, rest),
        };
        Parts {
            scheme,
            authority,
            path,
            query,
            fragment,
        }
    }
}

/// An authority split into user information, host and port.
#[derive(Clone, Copy, Debug, Default)]
pub struct Authority<'a> {
    pub userinfo: Option<&'a str>,
    pub host: &'a str,
    pub port: Option<&'a str>,
}

impl<'a> Authority<'a> {
    pub fn split(a: &'a str) -> Authority<'a> {
        let (userinfo, hostport) = match a.rfind('@') {
            Some(i) => (Some(&a[..i]), &a[i + 1..]),
            None => (None, a),
        };
        let host_end = if hostport.starts_with('[') {
            hostport.find(']').map_or(hostport.len(), |i| i + 1)
        } else {
            hostport.find(':').unwrap_or(hostport.len())
        };
        let (host, port) = (&hostport[..host_end], &hostport[host_end..]);
        Authority {
            userinfo,
            host,
            port: port.strip_prefix(':'),
        }
    }
}

/// Whether `c` may occur somewhere in an IRI reference (RFC 3987 §2.2): unreserved and
/// reserved ASCII characters, `%`, and the `ucschar` and `iprivate` ranges.
fn iri_char(c: char) -> bool {
    match c {
        'a'..='z' | 'A'..='Z' | '0'..='9' => true,
        '-' | '.' | '_' | '~' | ':' | '/' | '?' | '#' | '[' | ']' | '@' | '!' | '$' | '&'
        | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' | '%' => true,
        c if c.is_ascii() => false,
        c => {
            let u = c as u32;
            matches!(u,
                0xA0..=0xD7FF | 0xF900..=0xFDCF | 0xFDF0..=0xFFEF
                | 0x10000..=0x1FFFD | 0x20000..=0x2FFFD | 0x30000..=0x3FFFD
                | 0x40000..=0x4FFFD | 0x50000..=0x5FFFD | 0x60000..=0x6FFFD
                | 0x70000..=0x7FFFD | 0x80000..=0x8FFFD | 0x90000..=0x9FFFD
                | 0xA0000..=0xAFFFD | 0xB0000..=0xBFFFD | 0xC0000..=0xCFFFD
                | 0xD0000..=0xDFFFD | 0xE1000..=0xEFFFD
                | 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
        }
    }
}

/// The character position (from 1) and the character of the first character that can
/// never occur in an IRI, or of a `%` not followed by two hex digits.
fn first_bad_char(s: &str) -> Option<(usize, char)> {
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !iri_char(c) {
            return Some((i + 1, c));
        }
        if c == '%'
            && !(chars.get(i + 1).is_some_and(char::is_ascii_hexdigit)
                && chars.get(i + 2).is_some_and(char::is_ascii_hexdigit))
        {
            return Some((i + 1, c));
        }
    }
    None
}

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// The `%XX` triplets of `s` with their decoded byte.
fn percent_triplets(s: &str) -> impl Iterator<Item = (&str, u8)> {
    let b = s.as_bytes();
    (0..b.len()).filter_map(move |i| {
        if b[i] != b'%' {
            return None;
        }
        let hex = s.get(i + 1..i + 3)?;
        let v = u8::from_str_radix(hex, 16).ok()?;
        Some((&s[i..i + 3], v))
    })
}

fn is_hex_lower(s: &str) -> bool {
    s.chars()
        .all(|c| c.is_ascii_digit() || matches!(c, 'a'..='f'))
}

/// `8-4-4-4-12` hex digits.
fn uuid_shape(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(g, n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Dotted decimal numbers without leading zeros (`1.3.6.1`).
fn oid_shape(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|n| {
            !n.is_empty()
                && n.chars().all(|c| c.is_ascii_digit())
                && (n == "0" || !n.starts_with('0'))
        })
}

/// A DID method-specific id character (W3C DID Core §3.1: `idchar` or `:`).
fn did_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '%')
}

/// The warnings of a syntactically valid IRI reference (spec G05 §4.1). Relative
/// references get only the generic rules.
pub fn iri_warnings(iri: &str, out: &mut Vec<Issue>) {
    let p = Parts::split(iri);
    let scheme_lc = p.scheme.map(str::to_ascii_lowercase);
    let scheme = scheme_lc.as_deref();
    if let Some(s) = p.scheme
        && s.chars().any(|c| c.is_ascii_uppercase())
    {
        out.push(Issue::warn(
            "scheme-case",
            format!("the scheme '{s}' is not in lower case"),
        ));
    }
    // percent-encodings
    let mut lower = false;
    let mut unres = Vec::new();
    for (t, v) in percent_triplets(iri) {
        if t[1..].chars().any(|c| c.is_ascii_lowercase()) {
            lower = true;
        }
        if unreserved(v) && !unres.contains(&t) {
            unres.push(t);
        }
    }
    if lower {
        out.push(Issue::warn(
            "percent-case",
            "a percent-encoding uses lower-case hex digits (write %3A, not %3a)",
        ));
    }
    if !unres.is_empty() {
        out.push(Issue::warn(
            "percent-unreserved",
            format!(
                "{} encodes an unreserved character, which should be written as is",
                unres.join(", ")
            ),
        ));
    }
    if let Some(a) = p.authority {
        let auth = Authority::split(a);
        if let Some(u) = auth.userinfo {
            if u.contains(':') {
                out.push(Issue::warn(
                    "userinfo",
                    "the authority holds a password (user:password@)",
                ));
            } else {
                out.push(Issue::warn(
                    "userinfo",
                    "the authority holds user information",
                ));
            }
        }
        if !auth.host.starts_with('[') {
            if auth.host.chars().any(|c| c.is_ascii_uppercase()) {
                out.push(Issue::warn(
                    "host-case",
                    format!("the host '{}' is not in lower case", auth.host),
                ));
            }
            let nums: Vec<&str> = auth.host.split('.').collect();
            if nums.len() == 4
                && nums
                    .iter()
                    .all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
                && nums.iter().any(|n| {
                    n.parse::<u32>().map_or(true, |v| v > 255)
                        || (n.len() > 1 && n.starts_with('0'))
                })
            {
                out.push(Issue::warn(
                    "ipv4",
                    format!(
                        "the host '{}' looks like an IPv4 address but is not one",
                        auth.host
                    ),
                ));
            }
        }
        match auth.port {
            Some("") => out.push(Issue::warn(
                "empty-port",
                "the authority ends with ':' and an empty port",
            )),
            Some(port) => {
                let default = match scheme {
                    Some("http") => Some("80"),
                    Some("https") => Some("443"),
                    _ => None,
                };
                if default == Some(port.trim_start_matches('0')) || default == Some(port) {
                    out.push(Issue::warn(
                        "default-port",
                        format!(
                            "port {port} is the default of {}: leave it out",
                            scheme.unwrap_or_default()
                        ),
                    ));
                }
            }
            None => {}
        }
        if matches!(scheme, Some("http" | "https")) && auth.host.is_empty() {
            out.push(Issue::warn(
                "http-host",
                format!("an {} IRI with an empty host", scheme.unwrap_or_default()),
            ));
        }
    } else if matches!(scheme, Some("http" | "https")) {
        out.push(Issue::warn(
            "http-host",
            format!(
                "an {} IRI without '//' and a host",
                scheme.unwrap_or_default()
            ),
        ));
    }
    if p.scheme.is_some() && p.path.split('/').any(|s| s == "." || s == "..") {
        out.push(Issue::warn(
            "dot-segments",
            "the path has '.' or '..' segments",
        ));
    }
    match scheme {
        Some("urn") => urn_warnings(iri, &p, out),
        Some("uuid") => {
            out.push(Issue::warn(
                "uuid-scheme",
                "'uuid:' is not a registered scheme: write urn:uuid:…",
            ));
            if !uuid_shape(p.path) {
                out.push(Issue::warn("uuid", "not a UUID (8-4-4-4-12 hex digits)"));
            }
        }
        Some("oid") => {
            out.push(Issue::warn(
                "oid-scheme",
                "'oid:' is not a registered scheme: write urn:oid:…",
            ));
            if !oid_shape(p.path) {
                out.push(Issue::warn("oid", "not an OID (dotted decimal numbers)"));
            }
        }
        Some("file") => {
            if p.authority.is_none() && !p.path.starts_with('/') {
                out.push(Issue::warn(
                    "file",
                    "a file: IRI with a relative path (write file:///absolute/path)",
                ));
            }
        }
        Some("did") => {
            let ok = match p.path.split_once(':') {
                Some((method, id)) => {
                    !method.is_empty()
                        && method
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                        && !id.is_empty()
                        && !id.ends_with(':')
                        && id.chars().all(did_char)
                }
                None => false,
            };
            if !ok || p.authority.is_some() {
                out.push(Issue::warn(
                    "did",
                    "not a DID (did:method:method-specific-id, with a lower-case method)",
                ));
            }
        }
        _ => {}
    }
}

fn urn_warnings(iri: &str, p: &Parts<'_>, out: &mut Vec<Issue>) {
    if !iri.is_ascii() {
        out.push(Issue::warn(
            "urn-ascii",
            "a URN with characters outside ASCII",
        ));
    }
    let Some((nid, nss)) = p.path.split_once(':') else {
        out.push(Issue::warn(
            "urn-syntax",
            "a URN needs a namespace and a specific string (urn:NID:NSS)",
        ));
        return;
    };
    let nid_ok = (2..=32).contains(&nid.len())
        && nid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !nid.starts_with('-')
        && !nid.ends_with('-');
    if !nid_ok || nss.is_empty() || p.authority.is_some() {
        out.push(Issue::warn(
            "urn-syntax",
            format!("a malformed URN namespace '{nid}' or an empty specific string"),
        ));
        return;
    }
    let nid_lc = nid.to_ascii_lowercase();
    if nid_lc.starts_with("x-") {
        out.push(Issue::warn(
            "urn-x",
            format!("the experimental URN namespace '{nid}' is not allowed by RFC 8141"),
        ));
    }
    match nid_lc.as_str() {
        "uuid" => {
            if !uuid_shape(nss) {
                out.push(Issue::warn("uuid", "not a UUID (8-4-4-4-12 hex digits)"));
            } else if !is_hex_lower(&nss.replace('-', "")) {
                out.push(Issue::warn("uuid", "a UUID is written in lower case"));
            }
            if p.query.is_some() || p.fragment.is_some() {
                out.push(Issue::warn(
                    "uuid",
                    "a urn:uuid: IRI with a query or a fragment",
                ));
            }
        }
        "oid" if !oid_shape(nss) => {
            out.push(Issue::warn("oid", "not an OID (dotted decimal numbers)"));
        }
        _ => {}
    }
}

/// The RFC 3986 §6.2.2 and §6.2.3 normal form of an absolute IRI: lower-case scheme and
/// host, upper-case percent-encodings, unreserved characters decoded, dot segments
/// removed, an empty or default port dropped, and `/` for an empty http path.
pub fn normalize_iri(iri: &str) -> Option<String> {
    let p = Parts::split(iri);
    let scheme = p.scheme?.to_ascii_lowercase();
    let pct = |s: &str| -> String {
        let mut out = String::with_capacity(s.len());
        let b = s.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'%'
                && let Some(v) = s
                    .get(i + 1..i + 3)
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                if unreserved(v) {
                    out.push(v as char);
                } else {
                    out.push('%');
                    out.push_str(&s[i + 1..i + 3].to_ascii_uppercase());
                }
                i += 3;
                continue;
            }
            let c = s[i..].chars().next().unwrap_or_default();
            out.push(c);
            i += c.len_utf8().max(1);
        }
        out
    };
    let mut s = format!("{scheme}:");
    if let Some(a) = p.authority {
        let auth = Authority::split(a);
        s.push_str("//");
        if let Some(u) = auth.userinfo {
            s.push_str(&pct(u));
            s.push('@');
        }
        if auth.host.starts_with('[') {
            s.push_str(&auth.host.to_ascii_lowercase());
        } else {
            s.push_str(&pct(&auth.host.to_ascii_lowercase()));
        }
        let default = match scheme.as_str() {
            "http" => Some("80"),
            "https" => Some("443"),
            _ => None,
        };
        if let Some(port) = auth.port.filter(|p| !p.is_empty() && Some(*p) != default) {
            s.push(':');
            s.push_str(port);
        }
    }
    let path = remove_dot_segments(&pct(p.path));
    if path.is_empty() && p.authority.is_some() && matches!(scheme.as_str(), "http" | "https") {
        s.push('/');
    } else {
        s.push_str(&path);
    }
    if let Some(q) = p.query {
        s.push('?');
        s.push_str(&pct(q));
    }
    if let Some(f) = p.fragment {
        s.push('#');
        s.push_str(&pct(f));
    }
    oxiri::Iri::parse(s.as_str()).is_ok().then_some(s)
}

/// RFC 3986 §5.2.4: the path without `.` and `..` segments.
fn remove_dot_segments(path: &str) -> String {
    let mut input = path;
    let mut out: Vec<&str> = Vec::new();
    while !input.is_empty() {
        if let Some(r) = input
            .strip_prefix("../")
            .or_else(|| input.strip_prefix("./"))
        {
            input = r;
        } else if input.starts_with("/./") {
            input = &input[2..];
        } else if input == "/." {
            input = "/";
        } else if input.starts_with("/../") {
            input = &input[3..];
            out.pop();
        } else if input == "/.." {
            input = "/";
            out.pop();
        } else if input == "." || input == ".." {
            input = "";
        } else {
            // the first segment, with its leading `/`
            let start = usize::from(input.starts_with('/'));
            let end = input[start..].find('/').map_or(input.len(), |i| i + start);
            out.push(&input[..end]);
            input = &input[end..];
        }
    }
    out.concat()
}

/// What `sparkles iri` reports about one input.
pub struct IriReport {
    pub input: String,
    pub absolute: bool,
    pub valid: bool,
    /// scheme, authority, user info, host, port, path, query, fragment
    pub components: Vec<(&'static str, Option<String>)>,
    pub resolved: Option<String>,
    pub normalized: Option<String>,
    pub issues: Vec<Issue>,
}

/// Analyze an IRI (angle brackets allowed), resolved against `base` when it is relative.
pub fn analyze_iri(input: &str, base: Option<&str>) -> IriReport {
    let s = input
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(input)
        .to_string();
    let mut issues = Vec::new();
    let parsed = oxiri::IriRef::parse(s.as_str());
    let (valid, absolute) = match &parsed {
        Ok(r) => (true, r.is_absolute()),
        Err(e) => {
            let at = first_bad_char(&s)
                .map(|(i, _)| format!(" at character {i}"))
                .unwrap_or_default();
            issues.push(Issue::error("syntax", format!("{e}{at}")));
            (false, false)
        }
    };
    let mut resolved = None;
    if valid {
        if !absolute {
            issues.push(Issue::warn(
                "relative",
                "a relative reference, not an IRI: it needs a base",
            ));
        }
        iri_warnings(&s, &mut issues);
        if !absolute && let Some(b) = base {
            match oxiri::Iri::parse(b) {
                Ok(b) => match b.resolve(&s) {
                    Ok(r) => resolved = Some(r.into_inner()),
                    Err(e) => issues.push(Issue::error("syntax", format!("resolving: {e}"))),
                },
                Err(e) => issues.push(Issue::error("syntax", format!("--base: {e}"))),
            }
        }
    }
    let target = resolved.clone().unwrap_or_else(|| s.clone());
    let normalized = (valid && (absolute || resolved.is_some()))
        .then(|| normalize_iri(&target))
        .flatten()
        .filter(|n| *n != target);
    let mut components = Vec::new();
    if valid {
        let p = Parts::split(&s);
        let own = |v: Option<&str>| v.map(str::to_string);
        components.push(("scheme", own(p.scheme)));
        components.push(("authority", own(p.authority)));
        if let Some(a) = p.authority.map(Authority::split) {
            if a.userinfo.is_some() {
                components.push(("user info", own(a.userinfo)));
            }
            components.push(("host", Some(a.host.to_string())));
            if a.port.is_some() {
                components.push(("port", own(a.port)));
            }
        }
        components.push(("path", Some(p.path.to_string())));
        components.push(("query", own(p.query)));
        components.push(("fragment", own(p.fragment)));
    }
    IriReport {
        input: s,
        absolute,
        valid,
        components,
        resolved,
        normalized,
        issues,
    }
}

impl IriReport {
    pub fn has_errors(&self) -> bool {
        self.issues.iter().any(|i| i.error)
    }
    pub fn has_warnings(&self) -> bool {
        self.issues.iter().any(|i| !i.error)
    }

    pub fn text(&self) -> String {
        let mut s = format!("<{}>\n", self.input);
        let row = |s: &mut String, k: &str, v: &str| s.push_str(&format!("  {k:<14}{v}\n"));
        if self.valid {
            row(
                &mut s,
                "kind:",
                if self.absolute {
                    "absolute IRI"
                } else {
                    "relative reference"
                },
            );
        }
        // values between bars, so that empty ones show; `-` for an absent one
        for (k, v) in &self.components {
            let v = v.as_ref().map_or("-".to_string(), |v| format!("|{v}|"));
            row(&mut s, &format!("{k}:"), &v);
        }
        if let Some(r) = &self.resolved {
            row(&mut s, "resolved:", &format!("<{r}>"));
        }
        if let Some(n) = &self.normalized {
            row(&mut s, "normalized:", &format!("<{n}>"));
        }
        for i in &self.issues {
            let kind = if i.error { "error" } else { "warning" };
            s.push_str(&format!("  {kind}: {} [{}]\n", i.message, i.code));
        }
        if self.issues.is_empty() {
            s.push_str("  ok\n");
        }
        s
    }

    pub fn json(&self) -> J {
        let mut o = json!({
            "input": self.input,
            "valid": self.valid,
            "absolute": self.absolute,
            "issues": self.issues.iter().map(Issue::json).collect::<Vec<_>>(),
        });
        for (k, v) in &self.components {
            o[k.replace(' ', "")] = json!(v);
        }
        if let Some(r) = &self.resolved {
            o["resolved"] = json!(r);
        }
        if let Some(n) = &self.normalized {
            o["normalized"] = json!(n);
        }
        o
    }
}

// ---------------------------------------------------------------- language tags ----

const GRANDFATHERED: [&str; 26] = [
    "art-lojban",
    "cel-gaulish",
    "en-gb-oed",
    "i-ami",
    "i-bnn",
    "i-default",
    "i-enochian",
    "i-hak",
    "i-klingon",
    "i-lux",
    "i-mingo",
    "i-navajo",
    "i-pwn",
    "i-tao",
    "i-tay",
    "i-tsu",
    "no-bok",
    "no-nyn",
    "sgn-be-fr",
    "sgn-be-nl",
    "sgn-ch-de",
    "zh-guoyu",
    "zh-hakka",
    "zh-min",
    "zh-min-nan",
    "zh-xiang",
];

/// What `sparkles langtag` reports about one input.
pub struct LangReport {
    pub input: String,
    pub direction: Option<String>,
    pub canonical: Option<String>,
    pub subtags: Vec<(&'static str, String)>,
    pub issues: Vec<Issue>,
}

/// BCP 47's case conventions, from oxilangtag: the language and most subtags in lower
/// case, a script in title case, a region in upper case. `None` for a malformed tag.
pub fn canonical_case(tag: &str) -> Option<String> {
    oxilangtag::LanguageTag::parse_and_normalize(tag)
        .ok()
        .map(oxilangtag::LanguageTag::into_inner)
}

/// The warnings of a well-formed language tag (spec G05 §4.2), without `case`.
pub fn langtag_warnings(tag: &str, out: &mut Vec<Issue>) {
    let lc = tag.to_ascii_lowercase();
    if GRANDFATHERED.contains(&lc.as_str()) {
        out.push(Issue::warn(
            "grandfathered",
            format!("'{tag}' is a grandfathered tag, kept for compatibility only"),
        ));
        return;
    }
    let Ok(t) = oxilangtag::LanguageTag::parse(tag) else {
        return;
    };
    if let Some(ext) = t.extended_language() {
        out.push(Issue::warn(
            "extlang",
            format!(
                "the extended language subtag '{ext}': the preferred tag uses '{}' as the language",
                ext.rsplit('-').next().unwrap_or(ext)
            ),
        ));
    }
    let lang = t.primary_language();
    if (4..=8).contains(&lang.len()) && !lc.starts_with("x-") {
        out.push(Issue::warn(
            "reserved-language",
            format!(
                "the primary language '{lang}' has {} letters: {}",
                lang.len(),
                if lang.len() == 4 {
                    "four-letter subtags are reserved"
                } else {
                    "such subtags are rarely registered"
                }
            ),
        ));
    }
}

/// Analyze a language tag (`@` and an RDF 1.2 direction suffix allowed).
pub fn analyze_langtag(input: &str) -> LangReport {
    let raw = input.strip_prefix('@').unwrap_or(input);
    let mut issues = Vec::new();
    let (tag, direction) = match raw.split_once("--") {
        Some((t, d)) => (t.to_string(), Some(d.to_string())),
        None => (raw.to_string(), None),
    };
    if let Some(d) = &direction
        && d != "ltr"
        && d != "rtl"
    {
        issues.push(Issue::error(
            "direction",
            format!("the base direction '{d}' is neither ltr nor rtl"),
        ));
    }
    let mut subtags = Vec::new();
    let mut canonical = None;
    if tag.is_empty() {
        issues.push(Issue::error("syntax", "an empty language tag"));
    } else if tag.chars().any(char::is_whitespace) {
        issues.push(Issue::error(
            "syntax",
            "the language tag contains white space",
        ));
    } else {
        match oxilangtag::LanguageTag::parse(tag.as_str()) {
            Err(e) => issues.push(Issue::error("syntax", format!("not a BCP 47 tag: {e}"))),
            Ok(t) => {
                let mut put = |k: &'static str, v: Option<&str>| {
                    if let Some(v) = v.filter(|v| !v.is_empty()) {
                        subtags.push((k, v.to_string()));
                    }
                };
                put("language", Some(t.primary_language()));
                put("extlang", t.extended_language());
                put("script", t.script());
                put("region", t.region());
                put("variant", t.variant());
                put("extension", t.extension());
                put("private use", t.private_use());
                canonical = canonical_case(&tag);
                if let Some(c) = &canonical
                    && *c != tag
                {
                    issues.push(Issue::warn(
                        "case",
                        format!("not in the canonical case '{c}' (the case does not change the meaning in RDF)"),
                    ));
                }
                langtag_warnings(&tag, &mut issues);
            }
        }
    }
    LangReport {
        input: input.to_string(),
        direction,
        canonical,
        subtags,
        issues,
    }
}

impl LangReport {
    pub fn has_errors(&self) -> bool {
        self.issues.iter().any(|i| i.error)
    }
    pub fn has_warnings(&self) -> bool {
        self.issues.iter().any(|i| !i.error)
    }

    pub fn text(&self) -> String {
        let mut s = format!("{}\n", self.input);
        let row = |s: &mut String, k: &str, v: &str| s.push_str(&format!("  {k:<14}{v}\n"));
        if let Some(c) = &self.canonical {
            let dir = self
                .direction
                .as_ref()
                .map(|d| format!("--{d}"))
                .unwrap_or_default();
            row(&mut s, "canonical:", &format!("{c}{dir}"));
        }
        for (k, v) in &self.subtags {
            row(&mut s, &format!("{k}:"), v);
        }
        if let Some(d) = &self.direction {
            row(&mut s, "direction:", d);
        }
        for i in &self.issues {
            let kind = if i.error { "error" } else { "warning" };
            s.push_str(&format!("  {kind}: {} [{}]\n", i.message, i.code));
        }
        if self.issues.is_empty() {
            s.push_str("  ok\n");
        }
        s
    }

    pub fn json(&self) -> J {
        let mut o = json!({
            "input": self.input,
            "issues": self.issues.iter().map(Issue::json).collect::<Vec<_>>(),
        });
        if let Some(c) = &self.canonical {
            o["canonical"] = json!(c);
        }
        for (k, v) in &self.subtags {
            o[k.replace(' ', "")] = json!(v);
        }
        if let Some(d) = &self.direction {
            o["direction"] = json!(d);
        }
        o
    }
}

// ------------------------------------------------------------- checking data ----

/// The warnings of the terms of parsed data (`convert --check`, `load --check`): each
/// distinct IRI or tag once, the first `limit` of them kept for printing.
pub struct TermChecker {
    seen: HashSet<String>,
    pub warnings: Vec<(String, Issue)>,
    /// distinct values with warnings, kept or not
    pub total: usize,
    limit: usize,
    scratch: Vec<Issue>,
}

impl TermChecker {
    pub fn new(limit: usize) -> TermChecker {
        TermChecker {
            seen: HashSet::new(),
            warnings: Vec::new(),
            total: 0,
            limit,
            scratch: Vec::new(),
        }
    }

    fn record(&mut self, value: String) {
        if self.scratch.is_empty() || !self.seen.insert(value.clone()) {
            self.scratch.clear();
            return;
        }
        self.total += 1;
        for i in self.scratch.drain(..) {
            if self.warnings.len() < self.limit {
                self.warnings.push((value.clone(), i));
            }
        }
    }

    pub fn iri(&mut self, iri: &str) {
        if self.seen.contains(iri) {
            return;
        }
        iri_warnings(iri, &mut self.scratch);
        // the key of an IRI is the IRI; a language tag's starts with `@`, which no IRI does
        self.record(iri.to_string());
    }

    pub fn langtag(&mut self, tag: &str) {
        let key = format!("@{tag}");
        if self.seen.contains(&key) {
            return;
        }
        langtag_warnings(tag, &mut self.scratch);
        self.record(key);
    }

    fn subject(&mut self, s: NamedOrBlankNodeRef<'_>) {
        if let NamedOrBlankNodeRef::NamedNode(n) = s {
            self.iri(n.as_str());
        }
    }

    fn term(&mut self, t: TermRef<'_>) {
        match t {
            TermRef::NamedNode(n) => self.iri(n.as_str()),
            TermRef::BlankNode(_) => {}
            TermRef::Literal(l) => {
                if let Some(lang) = l.language() {
                    self.langtag(lang);
                } else {
                    self.iri(l.datatype().as_str());
                }
            }
            TermRef::Triple(t) => {
                self.subject(t.subject.as_ref());
                self.iri(t.predicate.as_str());
                self.term(t.object.as_ref());
            }
        }
    }

    pub fn quad(&mut self, q: QuadRef<'_>) {
        self.subject(q.subject);
        self.iri(q.predicate.as_str());
        self.term(q.object);
        if let GraphNameRef::NamedNode(g) = q.graph_name {
            self.iri(g.as_str());
        }
    }

    /// Fold another checker's findings into this one.
    pub fn merge(&mut self, other: TermChecker) {
        // the kept values first, in the order they were found
        let mut order: Vec<String> = Vec::new();
        let mut issues: std::collections::HashMap<String, Vec<Issue>> = Default::default();
        for (v, i) in other.warnings {
            if !issues.contains_key(&v) {
                order.push(v.clone());
            }
            issues.entry(v).or_default().push(i);
        }
        let rest: Vec<String> = other
            .seen
            .into_iter()
            .filter(|v| !issues.contains_key(v))
            .collect();
        for v in order.into_iter().chain(rest) {
            if self.seen.contains(&v) {
                continue;
            }
            match issues.remove(&v) {
                Some(is) => {
                    self.scratch = is;
                    self.record(v);
                }
                // counted, but past the other checker's limit
                None => {
                    self.seen.insert(v);
                    self.total += 1;
                }
            }
        }
    }

    /// Print the findings on stderr as `name: warning: <iri>: message [code]` (or
    /// `@tag`).
    pub fn report(&self, name: &str) {
        for (v, i) in &self.warnings {
            let shown = if v.starts_with('@') {
                v.clone()
            } else {
                format!("<{v}>")
            };
            eprintln!("{name}: warning: {shown}: {} [{}]", i.message, i.code);
        }
        let shown: HashSet<&String> = self.warnings.iter().map(|(v, _)| v).collect();
        if self.total > shown.len() {
            eprintln!(
                "{name}: … and {} more values with warnings",
                self.total - shown.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(iri: &str) -> Vec<&'static str> {
        let mut v = Vec::new();
        iri_warnings(iri, &mut v);
        v.iter().map(|i| i.code).collect()
    }

    #[test]
    fn clean_iris_have_no_warnings() {
        for iri in [
            "http://example.org/a/b?c=d#e",
            "https://example.org:8443/",
            "urn:isbn:0451450523",
            "urn:uuid:f81d4fae-7dec-11d0-a765-00a0c91e6bf6",
            "urn:oid:1.3.6.1.4.1",
            "file:///tmp/data.ttl",
            "did:example:123456789abcdefghi",
            "mailto:someone@example.org",
            "http://[::1]:3030/ds",
            "http://192.168.0.1/",
            "http://example.org/%C3%A9",
        ] {
            assert_eq!(codes(iri), Vec::<&str>::new(), "{iri}");
        }
    }

    #[test]
    fn iri_rules() {
        assert_eq!(codes("HTTP://example.org/"), ["scheme-case"]);
        assert_eq!(codes("http://example.org/%c3%a9"), ["percent-case"]);
        assert_eq!(codes("http://example.org/%41"), ["percent-unreserved"]);
        assert_eq!(codes("http://Example.org/"), ["host-case"]);
        assert_eq!(codes("http://u:p@example.org/"), ["userinfo"]);
        assert_eq!(codes("http://example.org:/"), ["empty-port"]);
        assert_eq!(codes("http://example.org:80/"), ["default-port"]);
        assert_eq!(codes("https://example.org:443/"), ["default-port"]);
        assert_eq!(codes("http://example.org/a/../b"), ["dot-segments"]);
        assert_eq!(codes("http://300.1.1.1/"), ["ipv4"]);
        assert_eq!(codes("http:/path"), ["http-host"]);
        assert_eq!(codes("http:///path"), ["http-host"]);
        assert_eq!(codes("urn:x-foo:bar"), ["urn-x"]);
        assert_eq!(codes("urn:a:b"), ["urn-syntax"]);
        assert_eq!(codes("urn:isbn:"), ["urn-syntax"]);
        assert_eq!(codes("urn:isbn"), ["urn-syntax"]);
        assert_eq!(codes("urn:uuid:123"), ["uuid"]);
        assert_eq!(
            codes("urn:uuid:F81D4FAE-7DEC-11D0-A765-00A0C91E6BF6"),
            ["uuid"]
        );
        assert_eq!(
            codes("urn:uuid:f81d4fae-7dec-11d0-a765-00a0c91e6bf6#x"),
            ["uuid"]
        );
        assert_eq!(
            codes("uuid:f81d4fae-7dec-11d0-a765-00a0c91e6bf6"),
            ["uuid-scheme"]
        );
        assert_eq!(codes("urn:oid:1.02"), ["oid"]);
        assert_eq!(codes("oid:1.2"), ["oid-scheme"]);
        assert_eq!(codes("file:data.ttl"), ["file"]);
        assert_eq!(codes("did:Example:abc"), ["did"]);
        assert_eq!(codes("did:example:"), ["did"]);
        assert_eq!(codes("urn:isbn:é"), ["urn-ascii"]);
    }

    #[test]
    fn dot_segments() {
        for (a, b) in [
            ("/a/b/c/./../../g", "/a/g"),
            ("mid/content=5/../6", "mid/6"),
            ("/a/..", "/"),
            ("/../a", "/a"),
            ("/a/./b/", "/a/b/"),
            ("", ""),
        ] {
            assert_eq!(remove_dot_segments(a), b, "{a}");
        }
    }

    #[test]
    fn normal_forms() {
        assert_eq!(
            normalize_iri("HTTP://Example.ORG:80/a/./b/../c/%7e%3a").as_deref(),
            Some("http://example.org/a/c/~%3A")
        );
        assert_eq!(
            normalize_iri("https://example.org").as_deref(),
            Some("https://example.org/")
        );
        assert_eq!(normalize_iri("urn:isbn:1").as_deref(), Some("urn:isbn:1"));
    }

    #[test]
    fn iri_reports() {
        let r = analyze_iri("<http://Example.org:80/a/../b>", None);
        assert!(r.valid && r.absolute && !r.has_errors());
        let c: Vec<_> = r.issues.iter().map(|i| i.code).collect();
        assert_eq!(c, ["host-case", "default-port", "dot-segments"]);
        assert_eq!(r.normalized.as_deref(), Some("http://example.org/b"));
        let r = analyze_iri("http://example.org/a b", None);
        assert!(r.has_errors());
        assert!(
            r.issues[0].message.contains("at character 21"),
            "{}",
            r.issues[0].message
        );
        let r = analyze_iri("../x", Some("http://example.org/a/b"));
        assert!(!r.absolute && r.has_warnings() && !r.has_errors());
        assert_eq!(r.resolved.as_deref(), Some("http://example.org/x"));
        assert!(r.text().contains("relative reference"));
        assert_eq!(r.json()["resolved"], "http://example.org/x");
    }

    #[test]
    fn langtag_reports() {
        let r = analyze_langtag("zh-yue-hk");
        assert_eq!(r.canonical.as_deref(), Some("zh-yue-HK"));
        let c: Vec<_> = r.issues.iter().map(|i| i.code).collect();
        assert_eq!(c, ["case", "extlang"]);
        assert!(r.subtags.contains(&("extlang", "yue".to_string())));
        let r = analyze_langtag("@en-Latn-US--rtl");
        assert!(r.issues.is_empty(), "{:?}", r.issues);
        assert_eq!(r.direction.as_deref(), Some("rtl"));
        assert!(analyze_langtag("en--up").has_errors());
        assert!(analyze_langtag("not a tag").has_errors());
        assert!(analyze_langtag("en-a").has_errors());
        let c: Vec<_> = analyze_langtag("i-klingon")
            .issues
            .iter()
            .map(|i| i.code)
            .collect();
        assert_eq!(c, ["grandfathered"]);
        let c: Vec<_> = analyze_langtag("abcde")
            .issues
            .iter()
            .map(|i| i.code)
            .collect();
        assert_eq!(c, ["reserved-language"]);
        assert_eq!(analyze_langtag("en").json()["language"], "en");
    }

    #[test]
    fn checkers_report_each_value_once_and_merge() {
        let mut a = TermChecker::new(100);
        a.iri("HTTP://e/a");
        a.iri("HTTP://e/a");
        a.iri("http://e/ok");
        a.langtag("zh-yue");
        assert_eq!(a.total, 2);
        assert_eq!(a.warnings.len(), 2);
        let mut b = TermChecker::new(100);
        b.iri("HTTP://e/a");
        b.iri("HTTP://e/b");
        a.merge(b);
        assert_eq!(a.total, 3);
        assert_eq!(a.warnings.len(), 3);
        let mut small = TermChecker::new(1);
        small.iri("HTTP://e/a");
        small.iri("HTTP://e/b");
        assert_eq!((small.total, small.warnings.len()), (2, 1));
    }
}
