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

/// Whether `java.net.URL` parses the string: a scheme of letters, digits, `+`, `-` and
/// `.` that starts with a letter, whose protocol the JDK handles, and for `jar:` a
/// `!/` separator.
fn is_java_url(s: &str) -> bool {
    let Some((scheme, rest)) = s.split_once(':') else {
        return false;
    };
    let valid = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid {
        return false;
    }
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" | "ftp" | "file" | "mailto" | "jrt" => true,
        "jar" => rest.contains("!/"),
        _ => false,
    }
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

    #[test]
    fn a_comment_needs_whitespace_before_its_mark() {
        let (t, _) = terms("http://ex.org/p#frag\thttp://ex.org/x # note");
        assert_eq!(t, ["http://ex.org/p#frag\thttp://ex.org/x"]);
    }
}
