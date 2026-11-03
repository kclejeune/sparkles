//! The built-in SKOS shapes (spec U12, "SKOS and SKOS-XL thesaurus editor"): SHACL shapes
//! that lay out the editing forms of concept schemes, concepts, collections, ordered
//! collections and SKOS-XL labels, and that check the integrity conditions of the W3C SKOS
//! Reference. A form reads them with `shapes=skos` ([`crate::forms::ShapesSource::Skos`]),
//! the web UI's thesaurus editor runs them over a graph, and they can be installed as a
//! dataset's write guard like any other shapes.
//!
//! The integrity conditions are S9 (concept schemes are not concepts), S13 (preferred,
//! alternative and hidden labels are pairwise disjoint), S14 (one preferred label per
//! language tag), S27 (`skos:related` is disjoint with `skos:broaderTransitive`), S37
//! (collections are neither concepts nor concept schemes, and the semantic relations link
//! concepts) and S46 (`skos:exactMatch` is disjoint with `skos:broadMatch` and
//! `skos:relatedMatch`), with S51, S52 and S58 of the SKOS-XL appendix and the
//! consequences of S55 to S57, which make the literal forms of SKOS-XL labels count for
//! S13 and S14. Each result's message names its condition. A missing `rdf:type` on a
//! linked concept, scheme or collection member is reported at `sh:Info` severity, which a
//! write guard with the default threshold lets through.

/// The shapes as Turtle.
pub const SHAPES: &str = include_str!("shapes.ttl");

/// The namespace of the shapes, their groups and their constraints.
pub const SHAPES_NS: &str = "urn:x-sparkles:skos:";

/// The shapes, parsed once.
#[cfg(feature = "shacl")]
pub fn shapes() -> std::sync::Arc<sparkles_shacl::Shapes> {
    use std::sync::{Arc, OnceLock};
    static SHAPES_PARSED: OnceLock<Arc<sparkles_shacl::Shapes>> = OnceLock::new();
    SHAPES_PARSED
        .get_or_init(|| {
            Arc::new(
                sparkles_shacl::Shapes::parse(SHAPES, crate::io::RdfFormat::Turtle, None)
                    .expect("the built-in SKOS shapes parse"),
            )
        })
        .clone()
}
