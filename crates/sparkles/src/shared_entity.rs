//! Versioned A01 application-profile resources over ordinary RDF.
//!
//! The resources separate descriptions, entity anchors and scoped resolution views.
//! They do not compute identity matches, authenticate review or availability, protect
//! managed records, or maintain current assessments. Resolution fixtures are declared
//! expected states for later reference replay.

/// A read-only competency query for one explicitly selected authorized view.
#[derive(Clone, Copy, Debug)]
pub struct QueryResource {
    pub name: &'static str,
    pub query: &'static str,
}

/// Static vocabulary, structural shapes, matching policy and competency queries.
#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub version: u32,
    pub iri: &'static str,
    pub vocabulary: &'static str,
    pub shapes: &'static str,
    pub policy: &'static str,
    pub manifest: &'static str,
    pub competency_queries: &'static [QueryResource],
}

const V1: Profile = Profile {
    version: 1,
    iri: "urn:x-sparkles:representation:profile:1",
    vocabulary: include_str!("../resources/entity-representation/v1/vocabulary.ttl"),
    shapes: include_str!("../resources/entity-representation/v1/shapes.ttl"),
    policy: include_str!("../resources/entity-representation/v1/policies/default.ttl"),
    manifest: include_str!("../resources/entity-representation/v1/manifest.ttl"),
    competency_queries: &[
        QueryResource {
            name: "descriptions.rq",
            query: include_str!("../resources/entity-representation/v1/queries/descriptions.rq"),
        },
        QueryResource {
            name: "identifiers.rq",
            query: include_str!("../resources/entity-representation/v1/queries/identifiers.rq"),
        },
        QueryResource {
            name: "relations.rq",
            query: include_str!("../resources/entity-representation/v1/queries/relations.rq"),
        },
        QueryResource {
            name: "resolution.rq",
            query: include_str!("../resources/entity-representation/v1/queries/resolution.rq"),
        },
        QueryResource {
            name: "support-lineage.rq",
            query: include_str!("../resources/entity-representation/v1/queries/support-lineage.rq"),
        },
        QueryResource {
            name: "evolution.rq",
            query: include_str!("../resources/entity-representation/v1/queries/evolution.rq"),
        },
        QueryResource {
            name: "software-boundaries.rq",
            query: include_str!(
                "../resources/entity-representation/v1/queries/software-boundaries.rq"
            ),
        },
        QueryResource {
            name: "episode-observations.rq",
            query: include_str!(
                "../resources/entity-representation/v1/queries/episode-observations.rq"
            ),
        },
    ],
};

/// Select an explicit known resource version without opening or changing a dataset.
///
/// Unknown versions return `None` without an implicit fallback.
pub fn profile(version: u32) -> Option<&'static Profile> {
    (version == V1.version).then_some(&V1)
}
