//! Result-map writers: the Sparkles JSON report, the ShapeMap JSON result map, the
//! compact result map and Jena's text report.

use crate::error::NOT_IMPLEMENTED;
use crate::{PrefixMap, ResultMap};

/// An IRI as a prefixed name with the longest namespace of `prefixes` whose remainder
/// is a plain local name, or as `<iri>`.
pub fn compact_iri(iri: &str, prefixes: &PrefixMap) -> String {
    prefixes
        .iter()
        .filter(|(_, ns)| !ns.is_empty() && iri.starts_with(ns.as_str()))
        .filter(|(_, ns)| is_plain_local(&iri[ns.len()..]))
        .max_by_key(|(_, ns)| ns.len())
        .map_or_else(
            || format!("<{iri}>"),
            |(p, ns)| format!("{p}:{}", &iri[ns.len()..]),
        )
}

/// A local name that can be written after `prefix:` without escapes: letters, digits,
/// `_`, `-`, `.` (not last) and `:`, not starting with `-` or `.`.
fn is_plain_local(s: &str) -> bool {
    let ok = |c: char| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | ':');
    s.chars().all(ok) && !s.starts_with(['-', '.']) && !s.ends_with('.')
}

/// See [`ResultMap::to_json`].
pub fn to_json(r: &ResultMap) -> serde_json::Value {
    let _ = r;
    unimplemented!("JSON reports: {NOT_IMPLEMENTED}")
}

/// See [`ResultMap::to_shapemap_json`].
pub fn to_shapemap_json(r: &ResultMap) -> serde_json::Value {
    let _ = r;
    unimplemented!("ShapeMap JSON result maps: {NOT_IMPLEMENTED}")
}

/// See [`ResultMap::to_smap`].
pub fn to_smap(r: &ResultMap) -> String {
    let _ = r;
    unimplemented!("compact result maps: {NOT_IMPLEMENTED}")
}

/// See [`ResultMap::to_text`].
pub fn to_text(r: &ResultMap) -> String {
    let _ = r;
    unimplemented!("text reports: {NOT_IMPLEMENTED}")
}
