//! What a circuit is keyed by: the compiler version, the normalized query and the
//! access that reads it.

use super::*;

/// The version of the compiler, part of a checkpoint's fingerprint: a state written
/// by another compiler is not loaded.
pub(super) const COMPILER: &str = "f13-phase2-1";

/// Whether `access` has protections that depend on the data, with the reason the
/// whole query is then evaluated again (spec F13 §6.2).
pub fn whole_reason(access: Option<&Arc<GraphAccess>>) -> Option<&'static str> {
    access
        .and_then(|a| a.triples.as_ref())
        .filter(|t| t.hides() && !t.state_independent())
        .map(|_| "protections that depend on the data")
}

/// A stable hash of what `access` lets a reader see: equal for equal grants. Guards a
/// request decides at its snapshot count as keeping their graphs out, as they do in a
/// circuit ([`Circuit::new`]).
pub fn access_fingerprint(access: Option<&Arc<GraphAccess>>) -> u64 {
    let settled = access.map(|a| a.settled());
    match settled.as_ref() {
        None => 0,
        Some(a) if a.reads_everything() => 0,
        Some(a) => {
            let mut h = Sha256::new();
            h.update(a.read_key().as_bytes());
            h.update([0]);
            if let Some(t) = a.triples.as_ref().filter(|t| t.hides()) {
                h.update(format!("{:?}", t.rules).as_bytes());
                if !t.state_independent() {
                    h.update(format!("{:?}", t.caller).as_bytes());
                }
            }
            u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes")) | 1
        }
    }
}

/// Whether a reader with `caller` may read a view whose principal reads `view`: the
/// caller reads everything, or reads exactly what the principal reads (spec F13 §6.1).
/// Comparing grant sets for containment is left to Phase 2; equal grants and
/// unrestricted callers cover every case Phase 1 needs.
pub fn covers(caller: Option<&Arc<GraphAccess>>, view: Option<&Arc<GraphAccess>>) -> bool {
    let c = access_fingerprint(caller);
    c == 0 || c == access_fingerprint(view)
}

/// The query's text with the variables the parser names at random (aggregates' and
/// others' that the source `text` does not hold) renamed in order of appearance, so
/// that one query parsed twice gives one text.
pub(super) fn normalized(q: &Query, text: &str) -> String {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let in_source = |name: &str| {
        ['?', '$'].iter().any(|p| {
            let pat = format!("{p}{name}");
            text.match_indices(&pat).any(|(i, _)| {
                text[i + pat.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !word(c))
            })
        })
    };
    let shown = q.to_string();
    let mut out = String::with_capacity(shown.len());
    let mut names: Vec<String> = Vec::new();
    let mut rest = shown.as_str();
    while let Some(i) = rest.find('?') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        let len = after.find(|c: char| !word(c)).unwrap_or(after.len());
        let name = &after[..len];
        if len == 0 || in_source(name) {
            out.push('?');
            out.push_str(name);
        } else {
            let n = match names.iter().position(|x| x == name) {
                Some(n) => n,
                None => {
                    names.push(name.to_string());
                    names.len() - 1
                }
            };
            out.push_str(&format!("?_v{n}"));
        }
        rest = &after[len..];
    }
    out.push_str(rest);
    out
}

pub(super) fn key_of(
    q: &Query,
    text: &str,
    access: Option<&Arc<GraphAccess>>,
    whole: Option<&str>,
) -> u64 {
    let mut h = Sha256::new();
    h.update(COMPILER.as_bytes());
    h.update([0]);
    h.update(normalized(q, text).as_bytes());
    h.update([0]);
    h.update(access_fingerprint(access).to_le_bytes());
    h.update(whole.unwrap_or("").as_bytes());
    u64::from_le_bytes(h.finalize()[..8].try_into().expect("8 bytes"))
}

#[cfg(test)]
mod compiler_tests {
    use super::COMPILER;
    use sha2::{Digest, Sha256};

    /// Each compiler version with the fingerprint of the operator variant list it
    /// compiles to, oldest first. A change to `ops::Op` changes what a circuit's state
    /// holds, so it needs a new `COMPILER` and a new row here: a state checkpointed by
    /// the old compiler is then refused by its fingerprint and the view is built
    /// again, instead of being decoded into the wrong operators.
    const VERSIONS: &[(&str, &str)] = &[
        ("f13-phase1-2", "650d426b8dbb3bb3"),
        // a GRAPH inside GRAPH compiles to a recompute node
        ("f13-phase1-3", "650d426b8dbb3bb3"),
        // an EXISTS whose empty path starts at an outer variable compiles to a
        // recompute node
        ("f13-phase1-4", "650d426b8dbb3bb3"),
        // paths with + and * compile to the fixed-point operator
        ("f13-phase3-1", "928d8f6b0c9509b7"),
        // MIN, MAX, GROUP_CONCAT and SAMPLE compile to the group operator, and a
        // correlated EXISTS to the exists operator
        ("f13-phase2-1", "9910b68bd4b7e026"),
    ];

    /// The variants of `ops::Op` as the source declares them, without comments or
    /// whitespace.
    fn op_variants() -> String {
        let src = include_str!("../ops/mod.rs");
        let start = src
            .find("pub enum Op {")
            .expect("ops::Op is declared in ops/mod.rs");
        let body = &src[start..];
        let end = body.find("\n}").expect("the end of ops::Op");
        body[..end]
            .lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .flat_map(|l| l.split_whitespace())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn fingerprint() -> String {
        let h = Sha256::digest(op_variants().as_bytes());
        h[..8].iter().map(|b| format!("{b:02x}")).collect()
    }

    /// The operator variants and `COMPILER` change together.
    #[test]
    fn compiler_version_follows_the_operators() {
        let got = fingerprint();
        let (version, pinned) = VERSIONS.last().expect("a version");
        assert_eq!(
            (*version, *pinned),
            (COMPILER, got.as_str()),
            "ops::Op changed ({}): bump COMPILER in incr/runtime/key.rs and add \
             (\"<new COMPILER>\", \"{got}\") to VERSIONS",
            op_variants()
        );
        // a version names one variant list; a bump for another reason, such as a
        // change of an operator's state encoding, may keep the list
        for (i, (v, _)) in VERSIONS.iter().enumerate() {
            for (w, _) in &VERSIONS[i + 1..] {
                assert_ne!(v, w, "COMPILER {v:?} is listed twice");
            }
        }
    }

    /// The fingerprint covers the variants and nothing around them.
    #[test]
    fn the_fingerprint_reads_the_variant_list() {
        let v = op_variants();
        assert!(v.starts_with("pub enum Op {"), "{v}");
        assert!(v.contains("Bgp(Box<bgp::Bgp>),"), "{v}");
        assert!(v.contains("Template(Box<template::Template>),"), "{v}");
        assert!(!v.contains("impl"), "{v}");
    }
}
