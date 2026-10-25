//! Versioned valid-time RDF resources and typed stored-query definitions.
//!
//! These resources interpret reported validity without asserting a claim's target
//! proposition. They do not install an HTTP helper, normalize source dates, authenticate
//! availability receipts, or change recorded-state selection.

/// A query and its C16 definition, ready for explicit administrator installation.
#[derive(Clone, Copy, Debug)]
pub struct QueryResource {
    pub name: &'static str,
    pub query: &'static str,
    /// JSON stored-query definition with typed RDF-term parameters.
    pub definition: &'static str,
}

/// One explicitly selected static profile version.
#[derive(Clone, Copy, Debug)]
pub struct Profile {
    pub version: u32,
    pub iri: &'static str,
    pub vocabulary: &'static str,
    pub manifest: &'static str,
    pub shapes: &'static str,
    pub queries: &'static [QueryResource],
}

const V1: Profile = Profile {
    version: 1,
    iri: "urn:x-sparkles:valid-time:profile:1",
    vocabulary: include_str!("../resources/valid-time/v1/vocabulary.ttl"),
    manifest: include_str!("../resources/valid-time/v1/manifest.ttl"),
    shapes: include_str!("../resources/valid-time/v1/shapes.ttl"),
    queries: &[
        QueryResource {
            name: "temporal-active-claims",
            query: include_str!("../resources/valid-time/v1/queries/temporal-active-claims.rq"),
            definition: include_str!(
                "../resources/valid-time/v1/queries/temporal-active-claims.json"
            ),
        },
        QueryResource {
            name: "temporal-overlapping-claims",
            query: include_str!(
                "../resources/valid-time/v1/queries/temporal-overlapping-claims.rq"
            ),
            definition: include_str!(
                "../resources/valid-time/v1/queries/temporal-overlapping-claims.json"
            ),
        },
    ],
};

/// Select a known profile version without opening or changing a dataset.
///
/// Unknown versions return `None`; there is no implicit fallback.
pub fn profile(version: u32) -> Option<&'static Profile> {
    (version == V1.version).then_some(&V1)
}
