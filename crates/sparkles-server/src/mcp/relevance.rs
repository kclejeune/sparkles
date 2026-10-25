//! Lexical question matching over the caller-visible schema, independent of gold queries.
use std::collections::BTreeSet;

/// The lowercase words of `s` of three letters or more, split at camel case, with a
/// plural `s` removed.
pub(crate) fn words(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    let mut push = |w: &mut String| {
        if w.chars().count() >= 3 {
            let mut x = w.to_lowercase();
            if x.len() > 3 && x.ends_with('s') && !x.ends_with("ss") {
                x.pop();
            }
            out.insert(x);
        }
        w.clear();
    };
    for c in s.chars() {
        if c.is_alphanumeric() {
            if c.is_uppercase() && prev_lower {
                push(&mut cur);
            }
            prev_lower = c.is_lowercase();
            cur.push(c);
        } else {
            push(&mut cur);
            prev_lower = false;
        }
    }
    push(&mut cur);
    out
}

/// Score an unrendered schema entry before applying the response's size limit.
pub(crate) fn entry_score<'a>(
    iri: &str,
    labels: impl Iterator<Item = &'a str>,
    q: &BTreeSet<String>,
) -> usize {
    let local = iri.rsplit(['#', '/', ':']).next().unwrap_or(iri);
    let mut tokens = words(local);
    for label in labels {
        tokens.extend(words(label));
    }
    tokens.intersection(q).count()
}
