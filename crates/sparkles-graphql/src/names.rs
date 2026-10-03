//! GraphQL names derived from IRIs (§4.1) and the names the server generates.

use sha2::{Digest, Sha256};

/// The local name of an IRI: the part after the last `#`, `/` or `:`, or the whole IRI
/// when that part is empty.
pub fn local_name(iri: &str) -> &str {
    let tail = iri.rsplit(['#', '/', ':']).next().unwrap_or("");
    if tail.is_empty() { iri } else { tail }
}

/// Step 2 of §4.1: characters outside `[_0-9A-Za-z]` become `_`, runs of `_` collapse,
/// leading underscores reduce to one, and a leading digit gets `_` in front.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '_' {
            c
        } else {
            '_'
        };
        if c == '_' && out.ends_with('_') {
            continue;
        }
        out.push(c);
    }
    if out.is_empty() {
        out.push('_');
    }
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    // `__` is reserved for introspection; runs are collapsed already
    out
}

/// The first six hexadecimal digits of the SHA-256 of an IRI.
pub fn short_hash(iri: &str) -> String {
    Sha256::digest(iri.as_bytes())
        .iter()
        .take(3)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The prefix of the dataset's prefixes whose namespace starts `iri` (the longest one).
pub fn prefix_of<'a>(iri: &str, prefixes: &'a [(String, String)]) -> Option<&'a str> {
    prefixes
        .iter()
        .filter(|(p, ns)| !ns.is_empty() && iri.starts_with(ns.as_str()) && !p.is_empty())
        .max_by_key(|(_, ns)| ns.len())
        .map(|(p, _)| p.as_str())
}

/// Names for a set of IRIs in one scope, by the four steps of §4.1. `reserved` names
/// count as taken.
pub fn assign(
    iris: &[String],
    prefixes: &[(String, String)],
    reserved: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let items: Vec<(String, String)> = iris
        .iter()
        .map(|i| (i.clone(), sanitize(local_name(i))))
        .collect();
    assign_bases(&items, prefixes, reserved)
}

/// [`assign`] with the step-2 name of each IRI given (`sh:name`, an inverse's `…Of`).
pub fn assign_bases(
    items: &[(String, String)],
    prefixes: &[(String, String)],
    reserved: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let base: Vec<String> = items.iter().map(|(_, b)| b.clone()).collect();
    let mut out = base.clone();
    let count = |names: &[String], n: &str| names.iter().filter(|x| *x == n).count();
    // step 3: colliding names take their prefix
    for i in 0..items.len() {
        if count(&base, &base[i]) > 1 || reserved(&base[i]) {
            out[i] = match prefix_of(&items[i].0, prefixes) {
                Some(p) => sanitize(&format!("{p}_{}", base[i])),
                None => format!("{}_{}", base[i], short_hash(&items[i].0)),
            };
        }
    }
    // step 4: names that still collide take a hash
    let snapshot = out.clone();
    for i in 0..items.len() {
        if count(&snapshot, &snapshot[i]) > 1 || reserved(&snapshot[i]) {
            out[i] = format!("{}_{}", snapshot[i], short_hash(&items[i].0));
        }
    }
    out
}

/// `birthDate` gives `BIRTH_DATE` (the values of `TOrderBy`).
pub fn upper_snake(name: &str) -> String {
    let mut out = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if c.is_ascii_uppercase() && prev_lower {
            out.push('_');
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// `Person` gives `person` (lookup root fields).
pub fn lower_first(name: &str) -> String {
    let mut c = name.chars();
    match c.next() {
        Some(f) => f.to_lowercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_names() {
        assert_eq!(
            sanitize(local_name("http://xmlns.com/foaf/0.1/Person")),
            "Person"
        );
        assert_eq!(
            sanitize(local_name("http://schema.org/birthDate")),
            "birthDate"
        );
        assert_eq!(sanitize(local_name("http://x.org/a#1st-name")), "_1st_name");
        assert_eq!(sanitize("__x"), "_x");
        assert_eq!(upper_snake("birthDate"), "BIRTH_DATE");
        assert_eq!(lower_first("Person"), "person");
    }

    #[test]
    fn collisions() {
        let p = vec![
            ("foaf".to_string(), "http://xmlns.com/foaf/0.1/".to_string()),
            ("ex".to_string(), "http://example.org/".to_string()),
        ];
        let names = assign(
            &[
                "http://xmlns.com/foaf/0.1/name".into(),
                "http://example.org/name".into(),
                "http://example.org/age".into(),
            ],
            &p,
            &|_| false,
        );
        assert_eq!(names, vec!["foaf_name", "ex_name", "age"]);
        let names = assign(
            &["http://a.org/x".into(), "http://b.org/x".into()],
            &[],
            &|_| false,
        );
        assert!(names[0].starts_with("x_") && names[0] != names[1]);
    }
}
