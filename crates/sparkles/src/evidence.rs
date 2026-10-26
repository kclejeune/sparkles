//! Versioned evidence-profile resources, independent of managed runtime operations.
//!
//! The vocabulary describes source revisions, unasserted claims and qualified
//! evidence. The shapes check RDF structure. They do not authenticate a record,
//! resolve a retained fact or protect a graph from ordinary writes. Those runtime
//! operations are separate from installing this resource profile.

/// One named SPARQL resource, ready to save or run against an authorized view.
#[derive(Clone, Copy, Debug)]
pub struct QueryResource {
    /// The stable file name within this profile's queries or compatibility directory.
    pub name: &'static str,
    /// The complete SPARQL query, including its namespace declarations.
    pub query: &'static str,
}

/// The vocabulary, shapes and queries of one explicit evidence-profile version.
#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub version: u32,
    /// Turtle vocabulary. Loading it does not enable a managed-write API.
    pub vocabulary: &'static str,
    /// Turtle resource identities and SHA-256 digests of the vocabulary, shapes and queries.
    pub manifest: &'static str,
    /// SHACL Core and SHACL-SPARQL shapes in Turtle.
    pub shapes: &'static str,
    /// SELECT queries that preserve claims, attribution and evidence distinctions.
    pub competency_queries: &'static [QueryResource],
    /// Read-only CONSTRUCT views of the existing C17 and C18 record formats.
    pub compatibility_queries: &'static [QueryResource],
}

const V1: Profile = Profile {
    version: 1,
    vocabulary: include_str!("../resources/evidence/v1/vocabulary.ttl"),
    manifest: include_str!("../resources/evidence/v1/manifest.ttl"),
    shapes: include_str!("../resources/evidence/v1/shapes.ttl"),
    competency_queries: &[
        QueryResource {
            name: "claim-occurrences.rq",
            query: include_str!("../resources/evidence/v1/queries/claim-occurrences.rq"),
        },
        QueryResource {
            name: "assertion-status.rq",
            query: include_str!("../resources/evidence/v1/queries/assertion-status.rq"),
        },
        QueryResource {
            name: "evidence-relations.rq",
            query: include_str!("../resources/evidence/v1/queries/evidence-relations.rq"),
        },
        QueryResource {
            name: "source-lineage.rq",
            query: include_str!("../resources/evidence/v1/queries/source-lineage.rq"),
        },
        QueryResource {
            name: "passage-selectors.rq",
            query: include_str!("../resources/evidence/v1/queries/passage-selectors.rq"),
        },
        QueryResource {
            name: "stored-fact-references.rq",
            query: include_str!("../resources/evidence/v1/queries/stored-fact-references.rq"),
        },
        QueryResource {
            name: "opposite-polarities.rq",
            query: include_str!("../resources/evidence/v1/queries/opposite-polarities.rq"),
        },
        QueryResource {
            name: "unsupported-claims.rq",
            query: include_str!("../resources/evidence/v1/queries/unsupported-claims.rq"),
        },
    ],
    compatibility_queries: &[
        QueryResource {
            name: "legacy-assertions.rq",
            query: include_str!("../resources/evidence/v1/compatibility/legacy-assertions.rq"),
        },
        QueryResource {
            name: "legacy-source-spans.rq",
            query: include_str!("../resources/evidence/v1/compatibility/legacy-source-spans.rq"),
        },
    ],
};

/// Obtain an explicitly supported resource version.
///
/// An unknown version returns `None`; it never selects a newer or older profile.
/// This selects static resources and does not open or change a dataset.
pub fn profile(version: u32) -> Option<&'static Profile> {
    (version == V1.version).then_some(&V1)
}
