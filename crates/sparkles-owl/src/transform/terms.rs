//! ROBOT 1.9.11's term lists, as `--term` and `--term-file` take them: `IOHelper`'s
//! `extractTerms`, one term a line with `#` comments, and its `createIRI`, which expands a
//! CURIE with ROBOT's default JSON-LD context and keeps the IRIs that `java.net.URL` or
//! ROBOT's URN pattern accepts. The context is `obo_context.jsonld` from ROBOT 1.9.11,
//! which the OBO Foundry registry generates: 282 prefixes such as `BFO`, `GO` and `RO`.

use rustc_hash::FxHashMap;
use std::collections::BTreeSet;
use std::sync::OnceLock;

/// ROBOT's default prefixes, by name.
fn context() -> &'static FxHashMap<String, String> {
    static CONTEXT: OnceLock<FxHashMap<String, String>> = OnceLock::new();
    CONTEXT.get_or_init(|| {
        let json: serde_json::Value = serde_json::from_str(include_str!("obo_context.jsonld"))
            .expect("the embedded OBO context is JSON");
        let mut out = FxHashMap::default();
        if let Some(ctx) = json.get("@context").and_then(|c| c.as_object()) {
            for (name, v) in ctx {
                let iri = match v {
                    serde_json::Value::String(s) => Some(s.as_str()),
                    v => v.get("@id").and_then(|i| i.as_str()),
                };
                if let Some(iri) = iri {
                    out.insert(name.clone(), iri.to_string());
                }
            }
        }
        out
    })
}

/// The IRIs of a term list, as ROBOT's `parseTerms` reads it, with a warning for each
/// term that is not an IRI ROBOT accepts, which it leaves out as ROBOT does.
pub fn parse_terms(text: &str, warnings: &mut Vec<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for t in extract_terms(text) {
        match create_iri(&t) {
            Some(iri) => {
                out.insert(iri);
            }
            None => warnings.push(format!("{t} is not a valid IRI.")),
        }
    }
    out
}

/// ROBOT's `extractTerms`: each line, without a line that starts with `#` and without a
/// comment that starts with whitespace and `#`, trimmed, when something is left.
fn extract_terms(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in text.replace('\r', "").split('\n') {
        if java_trim(line).starts_with('#') {
            continue;
        }
        // `replaceFirst("($|\\s)#.*$", "")`
        let chars: Vec<char> = line.chars().collect();
        let cut = (0..chars.len().saturating_sub(1))
            .find(|&i| is_java_space(chars[i]) && chars[i + 1] == '#')
            .unwrap_or(chars.len());
        let kept: String = chars[..cut].iter().collect();
        let t = java_trim(&kept);
        if !t.is_empty() {
            out.insert(t.to_string());
        }
    }
    out
}

/// Java's `\s`.
fn is_java_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r')
}

/// Java's `String.trim`, which drops every character up to U+0020 at both ends.
fn java_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c <= ' ')
}

/// ROBOT's `createIRI`: the term expanded as a JSON-LD key with the default context, then
/// kept when it is a URN of RFC 2141 or a URL `java.net.URL` parses.
fn create_iri(term: &str) -> Option<String> {
    let iri = expand(term)?;
    if iri.starts_with("urn:") {
        return is_urn(&iri).then_some(iri);
    }
    is_java_url(&iri).then_some(iri)
}

/// The JSON-LD 1.0 expansion of a key with `@vocab` unset: a term of the context, a
/// compact IRI with a prefix of the context, or an absolute IRI as it is. A term without
/// a colon expands to nothing, and a blank node identifier is not an IRI.
fn expand(term: &str) -> Option<String> {
    if term.starts_with('@') {
        return None;
    }
    let ctx = context();
    if let Some(iri) = ctx.get(term) {
        return Some(iri.clone());
    }
    let (prefix, suffix) = term.split_once(':')?;
    if prefix == "_" {
        return None;
    }
    if suffix.starts_with("//") {
        return Some(term.to_string());
    }
    match ctx.get(prefix) {
        Some(iri) => Some(format!("{iri}{suffix}")),
        None => Some(term.to_string()),
    }
}

/// ROBOT's URN pattern, `^urn:[a-z0-9][a-z0-9-]{0,31}:([a-z0-9()+,\-.:=@;$_!*']|%[0-9a-f]{2})+$`,
/// in any case.
fn is_urn(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("urn:") else {
        return false;
    };
    let Some((nid, nss)) = rest.split_once(':') else {
        return false;
    };
    let nid_ok = !nid.is_empty()
        && nid.len() <= 32
        && nid
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && nid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !nid_ok || nss.is_empty() {
        return false;
    }
    let b = nss.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'%' {
            if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
                i += 3;
                continue;
            }
            return false;
        }
        if c.is_ascii_alphanumeric() || b"()+,-.:=@;$_!*'".contains(&c) {
            i += 1;
            continue;
        }
        return false;
    }
    true
}

/// Whether `new java.net.URL(spec)` succeeds on OpenJDK 17, which ROBOT 1.9.11 runs on,
/// with the handlers of `java.base` and none of ROBOT's own. It follows the JDK's
/// `URL(URL, String, URLStreamHandler)`, `URLStreamHandler.parseURL` and `setURL`, and
/// the `mailto` and `jar` handlers' `parseURL`. The check fails on an unknown protocol, a
/// port that `Integer.parseInt` rejects or that is below -1, a bracketed host that is not
/// an IPv6 literal, a host with a control character, an empty `mailto` address and a
/// `jar` URL without `!/`, with an invalid inner URL or with a nested `jar:`.
fn is_java_url(spec: &str) -> bool {
    let b = spec.as_bytes();
    let mut limit = b.len();
    while limit > 0 && b[limit - 1] <= b' ' {
        limit -= 1;
    }
    let mut start = 0;
    while start < limit && b[start] <= b' ' {
        start += 1;
    }
    if b.len() >= start + 4 && b[start..start + 4].eq_ignore_ascii_case(b"url:") {
        start += 4;
    }
    // the protocol ends at the first `:` before any `/`, and a spec that starts with `#`
    // has none
    let mut protocol = None;
    if b.get(start) != Some(&b'#') {
        for i in start..limit {
            match b[i] {
                b'/' => break,
                b':' => {
                    protocol = Some(spec[start..i].to_lowercase());
                    start = i + 1;
                    break;
                }
                _ => {}
            }
        }
    }
    let Some(protocol) = protocol else {
        return false;
    };
    // the fragment
    let hash = spec[start..].find('#').map(|i| start + i);
    if let Some(i) = hash {
        limit = i;
    }
    match protocol.as_str() {
        "http" | "https" | "ftp" | "file" | "jrt" | "jmod" => default_parse_url(spec, start, limit),
        "mailto" => spec[start..limit.max(start)]
            .chars()
            .any(|c| !is_java_whitespace(c)),
        "jar" => {
            let sub = &spec[start..limit.max(start)];
            if sub.len() >= 4 && sub.as_bytes()[..4].eq_ignore_ascii_case(b"jar:") {
                return false; // nested jar URLs are not supported
            }
            if b.len() >= 4 && b[..4].eq_ignore_ascii_case(b"jar:") {
                // an absolute spec: the URL before its last `!/` must be one
                sub.rfind("!/").is_some_and(|i| is_java_url(&sub[..i]))
            } else {
                // a spec relative to a context (after `url:`): without a context only a
                // bare fragment parses
                hash.is_some() && start == limit
            }
        }
        _ => false,
    }
}

/// `URLStreamHandler.parseURL` of OpenJDK 17 with no context URL, then `setURL`'s host
/// check: whether the authority after `//` parses.
fn default_parse_url(spec: &str, mut start: usize, mut limit: usize) -> bool {
    let mut spec = spec;
    if start < limit
        && let Some(q) = spec.find('?')
        && q < limit
    {
        limit = q;
        spec = &spec[..q];
    }
    let b = spec.as_bytes();
    let unc = start + 4 <= limit && &b[start..start + 4] == b"////";
    let mut host = "";
    if !unc && start + 2 <= limit && &b[start..start + 2] == b"//" {
        start += 2;
        let i = find_byte(b, b'/', start)
            .filter(|&i| i <= limit)
            .or_else(|| find_byte(b, b'?', start).filter(|&i| i <= limit))
            .unwrap_or(limit);
        let authority = &spec[start..i];
        let server = match authority.find('@') {
            Some(at) if authority.rfind('@') != Some(at) => None,
            Some(at) => Some(&authority[at + 1..]),
            None => Some(authority),
        };
        let mut port = -1;
        if let Some(h) = server {
            if h.starts_with('[') {
                let Some(close) = h.find(']').filter(|&c| c > 2) else {
                    return false;
                };
                if !is_ipv6_literal(&h[1..close]) {
                    return false;
                }
                host = &h[..=close];
                match h.as_bytes().get(close + 1) {
                    None => {}
                    Some(b':') if h.len() > close + 2 => match java_parse_int(&h[close + 2..]) {
                        Some(p) => port = p,
                        None => return false,
                    },
                    Some(b':') => {}
                    Some(_) => return false,
                }
            } else if let Some(colon) = h.find(':') {
                if h.len() > colon + 1 {
                    match java_parse_int(&h[colon + 1..]) {
                        Some(p) => port = p,
                        None => return false,
                    }
                }
                host = &h[..colon];
            } else {
                host = h;
            }
        }
        if port < -1 {
            return false;
        }
    }
    // `IPAddressUtil.checkHostString`: no control character, `/`, or character that
    // normalizes to a delimiter
    const OTHERS: [u32; 17] = [
        8263, 8264, 8265, 8448, 8449, 8453, 8454, 10868, 65109, 65110, 65119, 65131, 65283, 65295,
        65306, 65311, 65312,
    ];
    !host
        .chars()
        .any(|c| c < ' ' || c == '\u{7f}' || c == '/' || OTHERS.contains(&(c as u32)))
}

fn find_byte(b: &[u8], x: u8, from: usize) -> Option<usize> {
    b.get(from..)?
        .iter()
        .position(|&c| c == x)
        .map(|i| from + i)
}

/// Java's `Integer.parseInt(s, 10)`: an optional sign and at least one decimal digit of
/// any script, within `int`.
fn java_parse_int(s: &str) -> Option<i64> {
    let first = s.chars().next()?;
    let (negative, digits) = match first {
        '-' => (true, &s[1..]),
        '+' => (false, &s[1..]),
        c if c < '0' => return None,
        _ => (false, s),
    };
    if digits.is_empty() {
        return None;
    }
    let mut v: i64 = 0;
    for c in digits.chars() {
        v = v * 10 + i64::from(java_decimal_digit(c)?);
        if v > 1 << 31 {
            return None;
        }
    }
    if negative {
        Some(-v)
    } else {
        (v < 1 << 31).then_some(v)
    }
}

/// Java's `Character.digit(c, 10)` for a UTF-16 unit: the decimal digits of the Basic
/// Multilingual Plane, in runs of ten from these zeros (Unicode 13, as in Java 17).
fn java_decimal_digit(c: char) -> Option<u32> {
    const ZEROS: [u32; 37] = [
        0x0030, 0x0660, 0x06F0, 0x07C0, 0x0966, 0x09E6, 0x0A66, 0x0AE6, 0x0B66, 0x0BE6, 0x0C66,
        0x0CE6, 0x0D66, 0x0DE6, 0x0E50, 0x0ED0, 0x0F20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946,
        0x19D0, 0x1A80, 0x1A90, 0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0,
        0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    ];
    let c = c as u32;
    ZEROS
        .iter()
        .find(|&&z| (z..z + 10).contains(&c))
        .map(|&z| c - z)
}

/// Java's `Character.isWhitespace` for a UTF-16 unit.
fn is_java_whitespace(c: char) -> bool {
    matches!(c,
        '\t'..='\r' | '\u{1c}'..=' ' | '\u{1680}' | '\u{2000}'..='\u{2006}'
        | '\u{2008}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{205f}' | '\u{3000}')
}

/// `IPAddressUtil.isIPv6LiteralAddress` of OpenJDK 17: eight groups of hexadecimal digits
/// with at most one `::`, an IPv4 address with three dots in place of the last two, and a
/// scope after `%`.
fn is_ipv6_literal(src: &str) -> bool {
    let s: Vec<char> = src.chars().collect();
    if s.len() < 2 {
        return false;
    }
    let mut end = s.len();
    if let Some(pc) = s.iter().position(|&c| c == '%') {
        if pc == s.len() - 1 {
            return false;
        }
        end = pc;
    }
    let (mut i, mut j) = (0, 0);
    if s[0] == ':' {
        i = 1;
        if s[1] != ':' {
            return false;
        }
    }
    let mut colonp = None;
    let mut curtok = i;
    let mut saw_xdigit = false;
    let mut val = 0u32;
    while i < end {
        let ch = s[i];
        i += 1;
        if let Some(d) = ch.to_digit(16) {
            val = (val << 4) | d;
            if val > 0xffff {
                return false;
            }
            saw_xdigit = true;
            continue;
        }
        if ch == ':' {
            curtok = i;
            if !saw_xdigit {
                if colonp.is_some() {
                    return false;
                }
                colonp = Some(j);
                continue;
            } else if i == end {
                return false;
            }
            if j + 2 > 16 {
                return false;
            }
            j += 2;
            saw_xdigit = false;
            val = 0;
            continue;
        }
        if ch == '.' && j + 4 <= 16 {
            let v4: String = s[curtok..end].iter().collect();
            if v4.matches('.').count() != 3 || !is_ipv4_literal(&v4) {
                return false;
            }
            j += 4;
            saw_xdigit = false;
            break;
        }
        return false;
    }
    if saw_xdigit {
        if j + 2 > 16 {
            return false;
        }
        j += 2;
    }
    if colonp.is_some() {
        if j == 16 {
            return false;
        }
        j = 16;
    }
    j == 16
}

/// `IPAddressUtil.textToNumericFormatV4` with ASCII digits, as OpenJDK 17 reads the
/// IPv4 part of an IPv6 literal.
fn is_ipv4_literal(s: &str) -> bool {
    let n = s.chars().count();
    if n == 0 || n > 15 {
        return false;
    }
    let (mut v, mut octet, mut new_octet) = (0u64, 0, true);
    for c in s.chars() {
        if c == '.' {
            if new_octet || v > 0xff || octet == 3 {
                return false;
            }
            octet += 1;
            v = 0;
            new_octet = true;
        } else {
            let Some(d) = c.to_digit(10) else {
                return false;
            };
            v = v * 10 + u64::from(d);
            new_octet = false;
        }
    }
    !new_octet && v < 1 << ((4 - octet) * 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(text: &str) -> (Vec<String>, Vec<String>) {
        let mut w = Vec::new();
        let t = parse_terms(text, &mut w).into_iter().collect();
        (t, w)
    }

    #[test]
    fn curies_expand_with_the_obo_context() {
        let (t, w) = terms("BFO:0000050\nRO:0000087 # has role\n# a comment\n\n  obo:GO_0005634  ");
        assert_eq!(
            t,
            [
                "http://purl.obolibrary.org/obo/BFO_0000050",
                "http://purl.obolibrary.org/obo/GO_0005634",
                "http://purl.obolibrary.org/obo/RO_0000087",
            ]
        );
        assert!(w.is_empty(), "{w:?}");
    }

    #[test]
    fn iris_and_urns_stay_and_the_rest_is_dropped_with_a_warning() {
        let (t, w) = terms(
            "http://ex.org/p\r\nurn:ex:p\nurn:-bad:p\nex:p\npart_of\n_:b\n<http://ex.org/q>\nhttps://ex.org/a#b",
        );
        assert_eq!(t, ["http://ex.org/p", "https://ex.org/a#b", "urn:ex:p"]);
        assert_eq!(
            w,
            [
                "<http://ex.org/q> is not a valid IRI.",
                "_:b is not a valid IRI.",
                "ex:p is not a valid IRI.",
                "part_of is not a valid IRI.",
                "urn:-bad:p is not a valid IRI.",
            ]
        );
    }

    /// Each spec with whether `new java.net.URL(spec)` succeeds on OpenJDK 17.0.20, the
    /// JDK of ROBOT 1.9.11's Nix package, as that JDK answered.
    #[test]
    fn urls_parse_as_openjdk_17_parses_them() {
        let cases = [
            ("http://ex.org:80/p", true),
            ("http://ex.org:/p", true),
            ("http://ex.org:+80/p", true),
            ("http://ex.org:-1/p", true),
            ("http://ex.org:\u{ff10}/p", true),
            ("http://ex.org:99999/p", true),
            ("http://a@b@c:bad/p", true),
            ("http://u:pw@ex.org:12/p", true),
            ("http://ex.org?x:bad", true),
            ("http://ex.org/p#:bad", true),
            ("http:ex.org:bad", true),
            ("http:////ex.org:bad", true),
            ("http:#x", true),
            ("http://[::1]:80/p", true),
            ("http://[fe80::1%eth0]/p", true),
            ("http://[::ffff:1.2.3.4]/p", true),
            ("http://[::01.2.3.4]/p", true),
            ("http://[00000::1]/p", true),
            ("http://ex.org/a b", true),
            ("HTTP://ex.org/p", true),
            ("url:http://ex.org/p", true),
            ("jmod:x", true),
            ("jrt:/java.base", true),
            ("mailto:a@b", true),
            ("jar:http://ex.org/a.jar!/p", true),
            ("jar:http://ex.org/a.jar!/p!/q", true),
            ("url:jar:#x", true),
            ("http://ex.org:bad/p", false),
            ("http://u@ex.org:bad/p", false),
            ("http://ex.org:-2/p", false),
            ("http://ex.org:2147483648/p", false),
            ("http://ex.org:+/p", false),
            ("http://ex.org:bad?q", false),
            ("http://[::1]x/p", false),
            ("http://[]/p", false),
            ("http://[12]/p", false),
            ("http://[::1/p", false),
            ("http://[fe80::1%]/p", false),
            ("http://[::1.2.3]/p", false),
            ("http://[12345::1]/p", false),
            ("http://[1::2::3]/p", false),
            ("http://[::\u{661}]/p", false),
            ("http://ex\u{2047}.org/p", false),
            ("http://ex\torg/p", false),
            ("mailto:", false),
            ("mailto:#x", false),
            ("mailto:\u{3000}", false),
            ("jar:foo!/x", false),
            ("jar:http://ex.org/a.jar", false),
            ("jar:http://ex.org:x/a.jar!/p", false),
            ("jar:jar:http://ex.org/a.jar!/b!/c", false),
            ("url:jar:http://ex.org/a.jar!/p", false),
            ("gopher://ex.org/p", false),
            ("1http://ex.org", false),
        ];
        for (spec, ok) in cases {
            assert_eq!(is_java_url(spec), ok, "{spec:?}");
        }
    }

    #[test]
    fn a_comment_needs_whitespace_before_its_mark() {
        let (t, _) = terms("http://ex.org/p#frag\thttp://ex.org/x # note");
        assert_eq!(t, ["http://ex.org/p#frag\thttp://ex.org/x"]);
    }
}
