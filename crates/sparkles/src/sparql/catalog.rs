//! The extension functions, aggregates and property functions this build implements,
//! by IRI: what a SPARQL 1.1 Service Description lists as `sd:extensionFunction`,
//! `sd:extensionAggregate` and `sd:propertyFeature`.
//!
//! The SPARQL 1.1 built-ins and the XSD casts are part of the language and not listed.
//! A test checks the lists against the functions' dispatchers.

use super::aggext::AFN;
use spargebra::algebra::{ARQ_AGGREGATE_KEYWORDS, ARQ_AGGREGATE_NAMESPACE};

/// XPath and XQuery Functions and Operators (`fn:`).
pub const FN: &str = "http://www.w3.org/2005/xpath-functions#";
/// XPath math functions (`math:`).
pub const MATH: &str = "http://www.w3.org/2005/xpath-functions/math#";

/// The `fn:` functions.
pub const FN_FUNCTIONS: &[&str] = &[
    "string-length",
    "substring",
    "upper-case",
    "lower-case",
    "contains",
    "starts-with",
    "ends-with",
    "substring-before",
    "substring-after",
    "concat",
    "string-join",
    "normalize-space",
    "normalize-unicode",
    "matches",
    "replace",
    "encode-for-uri",
    "abs",
    "ceiling",
    "floor",
    "round",
    "round-half-to-even",
    "numeric-mod",
    "numeric-integer-divide",
    "not",
    "boolean",
    "error",
    "dateTime",
    "year-from-dateTime",
    "month-from-dateTime",
    "day-from-dateTime",
    "hours-from-dateTime",
    "minutes-from-dateTime",
    "seconds-from-dateTime",
    "timezone-from-dateTime",
    "year-from-date",
    "month-from-date",
    "day-from-date",
    "timezone-from-date",
    "hours-from-time",
    "minutes-from-time",
    "seconds-from-time",
    "timezone-from-time",
    "years-from-duration",
    "months-from-duration",
    "days-from-duration",
    "hours-from-duration",
    "minutes-from-duration",
    "seconds-from-duration",
    "adjust-dateTime-to-timezone",
    "adjust-date-to-timezone",
    "adjust-time-to-timezone",
    "implicit-timezone",
    // ARQ's names for the date accessors
    "years-from-date",
    "months-from-date",
    "days-from-date",
    "years-from-dateTime",
    "months-from-dateTime",
    "days-from-dateTime",
];

/// The `math:` functions.
pub const MATH_FUNCTIONS: &[&str] = &[
    "pi", "e", "sqrt", "exp", "exp10", "log", "log10", "pow", "sin", "cos", "tan", "asin", "acos",
    "atan", "atan2",
];

/// Jena ARQ's `afn:` functions.
pub const AFN_FUNCTIONS: &[&str] = &[
    "localname",
    "namespace",
    "now",
    "sqrt",
    "pi",
    "e",
    "min",
    "max",
    "strjoin",
    "bnode",
    "strlen",
    "substr",
    "substring",
    "sha1sum",
    "uuid",
    "struuid",
    "evenInteger",
    "langeq",
    "date",
    "timezone",
    "adjust-to-timezone",
];

/// Sparkles' vector functions (`spk:`, `urn:x-sparkles:`).
pub const SPK_FUNCTIONS: &[&str] = &["cosine", "dot", "euclidean", "dimension"];

/// Every extension function IRI.
pub fn extension_functions() -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |ns: &str, locals: &[&str]| {
        out.extend(locals.iter().map(|l| format!("{ns}{l}")));
    };
    add(FN, FN_FUNCTIONS);
    add(MATH, MATH_FUNCTIONS);
    add(AFN, AFN_FUNCTIONS);
    add(crate::vector::NS, SPK_FUNCTIONS);
    #[cfg(feature = "geo")]
    {
        use crate::geo::vocab::{GEOF, Relation, SPATIALF};
        let relations: Vec<&str> = Relation::ALL.iter().map(|r| r.local()).collect();
        add(GEOF, &relations);
        add(GEOF, crate::geo::functions::FUNCTIONS);
        add(SPATIALF, crate::geo::spatialf::FUNCTIONS);
    }
    out
}

/// Every extension aggregate IRI: ARQ's statistical aggregates (`agg:`, and the
/// variance and deviation ones under `afn:` too) and the GeoSPARQL aggregates.
pub fn extension_aggregates() -> Vec<String> {
    let mut out: Vec<String> = ARQ_AGGREGATE_KEYWORDS
        .iter()
        .map(|(_, l)| format!("{ARQ_AGGREGATE_NAMESPACE}{l}"))
        .collect();
    out.extend(
        ARQ_AGGREGATE_KEYWORDS
            .iter()
            .filter(|(_, l)| !matches!(*l, "median" | "mode"))
            .map(|(_, l)| format!("{AFN}{l}")),
    );
    #[cfg(feature = "geo")]
    out.extend(
        crate::geo::vocab::AGGREGATES
            .iter()
            .map(|l| format!("{}{l}", crate::geo::vocab::GEOF)),
    );
    out
}

/// Every property function IRI: `text:query`, the vector and hybrid searches, and with
/// the `geo` feature Jena's `spatial:` property functions.
pub fn property_functions() -> Vec<String> {
    #[cfg_attr(not(feature = "geo"), allow(unused_mut))]
    let mut out = vec![
        super::textpf::TEXT_QUERY.to_string(),
        crate::vector::VECTOR_SEARCH.to_string(),
        super::hybrid::HYBRID_SEARCH.to_string(),
    ];
    #[cfg(feature = "geo")]
    out.extend(
        crate::geo::vocab::SpatialPfKind::ALL
            .iter()
            .map(|k| format!("{}{}", crate::geo::vocab::SPATIAL, k.local())),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The string literals that start the arms of the `match` after `start` in `src`, up
    /// to its fallback arm `end`.
    fn arms(src: &str, start: &str, end: &str) -> Vec<String> {
        let from = src.find(start).unwrap_or_else(|| panic!("{start}")) + start.len();
        let to = from + src[from..].find(end).unwrap_or_else(|| panic!("{end}"));
        let body = &src[from..to];
        let mut out = Vec::new();
        for line in body.lines() {
            let t = line.trim_start();
            // an arm: `"a" | "b" => …` or `"a" if … => …`
            if !t.starts_with('"') {
                continue;
            }
            let head = t.split("=>").next().unwrap_or("");
            for part in head.split('|') {
                let p = part.trim();
                let p = p.split(" if ").next().unwrap_or(p).trim();
                if let Some(s) = p.strip_prefix('"').and_then(|p| p.strip_suffix('"')) {
                    out.push(s.to_string());
                }
            }
        }
        out
    }

    fn listed(locals: &[&str], found: Vec<String>, what: &str) {
        assert!(
            found.len() >= 4,
            "{what}: only {found:?} found in the dispatcher"
        );
        for f in found {
            assert!(locals.contains(&f.as_str()), "{what}{f} is not listed");
        }
    }

    #[test]
    fn lists_every_dispatched_function() {
        let src = include_str!("expr.rs");
        listed(
            MATH_FUNCTIONS,
            arms(
                src,
                "iri.strip_prefix(MATH) {",
                "_ => Err(TypeError),\n        };",
            ),
            "math:",
        );
        listed(
            FN_FUNCTIONS,
            arms(
                src,
                "iri.strip_prefix(FN) {",
                "_ => Err(TypeError),\n        };",
            ),
            "fn:",
        );
        listed(
            AFN_FUNCTIONS,
            arms(
                src,
                "iri.strip_prefix(AFN) {",
                "_ => Err(TypeError),\n        };",
            ),
            "afn:",
        );
        listed(
            SPK_FUNCTIONS,
            arms(
                src,
                "iri.strip_prefix(crate::vector::NS) {",
                "_ => Err(TypeError),\n        };",
            ),
            "spk:",
        );
        #[cfg(feature = "geo")]
        {
            let geo = include_str!("../geo/functions.rs");
            listed(
                crate::geo::functions::FUNCTIONS,
                arms(geo, "Some(match local {", "_ => return None"),
                "geof:",
            );
            let sf = include_str!("../geo/spatialf.rs");
            listed(
                crate::geo::spatialf::FUNCTIONS,
                arms(sf, "Some(match local {", "_ => return None"),
                "spatialF:",
            );
        }
        // the lists name only functions that exist
        for iri in extension_functions() {
            assert!(super::super::expr::is_extension(&iri), "{iri}");
        }
    }

    #[test]
    fn aggregates_parse_as_aggregates() {
        let p = super::super::aggext::register(spargebra::SparqlParser::new());
        for iri in extension_aggregates() {
            let q = format!("SELECT (<{iri}>(?o) AS ?x) WHERE {{ ?s ?p ?o }}");
            p.clone()
                .parse_query(&q)
                .unwrap_or_else(|e| panic!("{iri}: {e}"));
        }
    }
}
