//! The registry of the W3C RDF syntaxes that oxrdfio reads: the names, file extensions
//! and media types that select each one.
//!
//! This table is the one place these aliases are declared. File names
//! ([`crate::io::format_for_path`]), media types ([`crate::io::format_for_media_type`]),
//! result format names ([`crate::sparql::results::rdf_format_from_name`]), Jena-style
//! syntax names ([`crate::io::DataSyntax::from_name`]) and the readers of the other
//! crates look a syntax up here, so that an alias that one of them accepts is accepted by
//! all of them. A caller that cannot handle a syntax, such as a writer and N3, leaves it
//! out after the lookup. Jena's own syntaxes are in [`crate::jena_formats`].

use oxrdfio::{JsonLdProfile, JsonLdProfileSet, RdfFormat};

/// One RDF syntax and what selects it. Names and extensions are lower case, and the
/// first media type is the one the syntax is written with.
#[derive(Clone, Copy, Debug)]
pub struct Syntax {
    pub format: RdfFormat,
    pub names: &'static [&'static str],
    pub extensions: &'static [&'static str],
    pub media_types: &'static [&'static str],
}

const JSON_LD: RdfFormat = RdfFormat::JsonLd {
    profile: JsonLdProfileSet::empty(),
};

/// Every syntax of the registry.
pub const SYNTAXES: &[Syntax] = &[
    Syntax {
        format: RdfFormat::Turtle,
        names: &["turtle", "ttl"],
        extensions: &["ttl", "turtle"],
        media_types: &["text/turtle", "application/x-turtle"],
    },
    Syntax {
        format: RdfFormat::NTriples,
        names: &["n-triples", "ntriples", "n-triple", "nt"],
        extensions: &["nt", "ntriples"],
        media_types: &["application/n-triples", "text/plain"],
    },
    Syntax {
        format: RdfFormat::NQuads,
        names: &["n-quads", "nquads", "n-quad", "nq"],
        extensions: &["nq", "nquads"],
        media_types: &["application/n-quads", "text/x-nquads"],
    },
    Syntax {
        format: RdfFormat::TriG,
        names: &["trig"],
        extensions: &["trig"],
        media_types: &["application/trig"],
    },
    Syntax {
        format: RdfFormat::RdfXml,
        names: &["rdf/xml", "rdfxml", "rdf-xml", "rdf", "xml"],
        extensions: &["rdf", "owl", "xml", "rdfxml"],
        media_types: &["application/rdf+xml", "application/xml", "text/xml"],
    },
    Syntax {
        format: JSON_LD,
        names: &["json-ld", "jsonld"],
        extensions: &["jsonld", "json"],
        media_types: &["application/ld+json", "application/json"],
    },
    Syntax {
        format: RdfFormat::N3,
        names: &["n3"],
        extensions: &["n3"],
        media_types: &["text/n3"],
    },
];

/// The names of streaming JSON-LD, which select JSON-LD with the streaming profile.
const STREAMING_NAMES: &[&str] = &["jsonld-streaming", "json-ld-streaming"];

/// The syntax of a name such as `turtle`, `n-triples` or `rdf/xml`, in any case.
pub fn by_name(name: &str) -> Option<RdfFormat> {
    let n = name.trim().to_ascii_lowercase();
    if STREAMING_NAMES.contains(&n.as_str()) {
        return Some(RdfFormat::JsonLd {
            profile: JsonLdProfile::Streaming.into(),
        });
    }
    SYNTAXES
        .iter()
        .find(|s| s.names.contains(&n.as_str()))
        .map(|s| s.format)
}

/// The syntax of a file extension without its dot, in any case.
pub fn by_extension(ext: &str) -> Option<RdfFormat> {
    let e = ext.trim_start_matches('.').to_ascii_lowercase();
    SYNTAXES
        .iter()
        .find(|s| s.extensions.contains(&e.as_str()))
        .map(|s| s.format)
}

/// The syntax of a media type, in any case and without its parameters, which
/// [`crate::io::format_for_media_type`] reads.
pub fn by_media_type(media_type: &str) -> Option<RdfFormat> {
    let base = media_type.split(';').next()?.trim().to_ascii_lowercase();
    SYNTAXES
        .iter()
        .find(|s| s.media_types.contains(&base.as_str()))
        .map(|s| s.format)
}

/// The registry's entry of `format`, whatever its JSON-LD profile.
pub fn of(format: RdfFormat) -> Option<&'static Syntax> {
    SYNTAXES.iter().find(|s| match (s.format, format) {
        (RdfFormat::JsonLd { .. }, RdfFormat::JsonLd { .. }) => true,
        (a, b) => a == b,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_alias_selects_one_syntax() {
        let mut names = std::collections::HashSet::new();
        let mut extensions = std::collections::HashSet::new();
        let mut media_types = std::collections::HashSet::new();
        for s in SYNTAXES {
            for n in s.names.iter().chain(STREAMING_NAMES) {
                assert_eq!(*n, n.to_ascii_lowercase());
            }
            assert!(s.names.iter().all(|n| names.insert(*n)), "{s:?}");
            assert!(s.extensions.iter().all(|e| extensions.insert(*e)), "{s:?}");
            assert!(
                s.media_types.iter().all(|m| media_types.insert(*m)),
                "{s:?}"
            );
            assert!(of(s.format).is_some_and(|o| o.format == s.format));
        }
    }

    #[test]
    fn lookups_ignore_case_dots_and_parameters() {
        assert_eq!(by_name("RDF/XML"), Some(RdfFormat::RdfXml));
        assert_eq!(by_name(" N-Triples "), Some(RdfFormat::NTriples));
        assert_eq!(by_extension(".OWL"), Some(RdfFormat::RdfXml));
        assert_eq!(by_extension("json"), Some(JSON_LD));
        assert_eq!(
            by_media_type("Text/Turtle; charset=utf-8"),
            Some(RdfFormat::Turtle)
        );
        assert_eq!(
            by_name("JSON-LD-streaming"),
            Some(RdfFormat::JsonLd {
                profile: JsonLdProfile::Streaming.into()
            })
        );
        assert_eq!(by_name("json"), None);
        assert_eq!(by_extension("txt"), None);
    }
}
