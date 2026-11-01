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

// ------------------------------------------------------------ settings ------------

pub use sparkles_core::evidence::{CanonicalSettings, ClaimKind, ConflictMode, Policy, Settings};

use crate::managed::{OwnerEntry, OwnerKind};
use crate::store::Store;
use crate::task::Control;
use crate::{Dataset, Error, Result};

/// A dataset's evidence settings and the protection they have now.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsStatus {
    /// The recorded settings, or the disabled defaults when the profile was never
    /// configured.
    pub settings: Settings,
    /// The SHA-256 digest of the recorded `evidence.json`, `None` when there is none.
    pub digest: Option<String>,
    /// The revision of the dataset's managed-graph registry, 0 when it has none.
    pub revision: u64,
    /// The graphs of the settings' roles that ordinary writes cannot change.
    pub managed: Vec<String>,
    /// The graphs of the settings' roles that an administrator has detached, which
    /// ordinary writes can change until the settings are configured again.
    pub detached: Vec<String>,
}

/// The evidence settings of `dataset`. Fails with `managed-unavailable` when the
/// dataset's managed-graph registry cannot be enforced.
pub fn settings(dataset: &Dataset) -> Result<SettingsStatus> {
    status_of(dataset.store())
}

fn status_of(store: &Store) -> Result<SettingsStatus> {
    let graphs = store.managed_graphs()?;
    let revision = graphs.as_ref().map_or(0, |g| g.revision);
    let protected: Vec<String> = graphs
        .as_ref()
        .and_then(|g| g.owners.get(&OwnerKind::Evidence))
        .map(|e| e.graphs.clone())
        .unwrap_or_default();
    let Some(bytes) = store.managed_settings(OwnerKind::Evidence)? else {
        return Ok(SettingsStatus {
            settings: Settings::default(),
            digest: None,
            revision,
            managed: Vec::new(),
            detached: Vec::new(),
        });
    };
    let settings = Settings::from_slice(&bytes, &Control::none()).map_err(|e| {
        crate::managed::ManagedError::error(
            crate::managed::ManagedErrorKind::Unavailable,
            format!(
                "{} cannot be read by this build: {e}",
                OwnerKind::Evidence.settings_file()
            ),
        )
    })?;
    let (managed, detached) = if settings.enabled {
        settings
            .managed_graphs()
            .into_iter()
            .partition(|g| protected.contains(g))
    } else {
        (Vec::new(), Vec::new())
    };
    Ok(SettingsStatus {
        settings,
        digest: Some(crate::managed::settings_digest(&bytes)),
        revision,
        managed,
        detached,
    })
}

/// Decode complete evidence settings from JSON. A `format` or `profile` this build
/// does not support refuses as `unsupported-profile` before the rest is interpreted.
pub fn parse_settings(bytes: &[u8], control: &Control) -> Result<Settings> {
    Settings::from_slice(bytes, control).map_err(unsupported_profile)
}

fn unsupported_profile(e: Error) -> Error {
    match e {
        Error::Unsupported(message) => crate::managed::ManagedError::error(
            crate::managed::ManagedErrorKind::UnsupportedProfile,
            message
                .strip_prefix("unsupported-profile: ")
                .unwrap_or(&message)
                .to_string(),
        ),
        e => e,
    }
}

/// Record `settings` as the evidence settings of `dataset` and protect the graphs of
/// their roles. Enabled settings hand every graph of every role to the evidence owner,
/// including graphs that an administrator detached earlier, and write the canonical
/// settings to `evidence.json`. The first configuration raises the dataset reader to
/// 6 durably.
///
/// Configuration never weakens protection. Settings that would leave a protected graph
/// without a role, or disable the profile while it protects a graph, are refused with
/// a conflict, and the graph must first be detached with [`detach`]. A disabled
/// configuration of a dataset that was never configured is refused unless it equals
/// the defaults, which then write nothing. A configuration that races with another
/// change of the dataset's managed-graph registry is refused with a conflict and
/// writes nothing.
///
/// Placing a graph under management does not authenticate what it already holds.
/// Typed evidence operations trust a record only through its verified publication.
pub fn configure(
    dataset: &Dataset,
    settings: &Settings,
    control: &Control,
) -> Result<SettingsStatus> {
    let store = dataset.store();
    let canonical = settings.canonical(control).map_err(unsupported_profile)?;
    let graphs = store.managed_graphs()?;
    let revision = graphs.as_ref().map_or(0, |g| g.revision);
    let current = graphs
        .as_ref()
        .and_then(|g| g.owners.get(&OwnerKind::Evidence));
    let roles = if settings.enabled {
        settings.managed_graphs()
    } else {
        Vec::new()
    };
    if let Some(current) = current
        && let Some(graph) = current.graphs.iter().find(|g| !roles.contains(g))
    {
        return Err(Error::Conflict(format!(
            "the evidence settings would stop protecting <{graph}>; detach it first"
        )));
    }
    if current.is_none() && !settings.enabled {
        let defaults = Settings::default().canonical(control)?;
        if canonical.bytes() == defaults.bytes() {
            return status_of(store);
        }
        return Err(Error::invalid(
            "the evidence profile is not enabled; enable it before storing disabled settings",
        ));
    }
    control.check()?;
    let entry = OwnerEntry::for_settings(settings.profile, roles, canonical.bytes());
    store.enable_managed_at(OwnerKind::Evidence, entry, canonical.bytes(), revision)?;
    status_of(store)
}

/// Stop protecting `graph`, a graph of the evidence settings' roles, so that ordinary
/// writes can change it. The settings keep the graph in its roles, and it is protected
/// again by the next [`configure`]. Records written to the graph while it is detached
/// are not authenticated by protecting it again.
pub fn detach(dataset: &Dataset, graph: &str) -> Result<SettingsStatus> {
    let store = dataset.store();
    store.detach_managed(OwnerKind::Evidence, graph)?;
    status_of(store)
}
