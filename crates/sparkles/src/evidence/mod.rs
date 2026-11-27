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
    vocabulary: include_str!("../../resources/evidence/v1/vocabulary.ttl"),
    manifest: include_str!("../../resources/evidence/v1/manifest.ttl"),
    shapes: include_str!("../../resources/evidence/v1/shapes.ttl"),
    competency_queries: &[
        QueryResource {
            name: "claim-occurrences.rq",
            query: include_str!("../../resources/evidence/v1/queries/claim-occurrences.rq"),
        },
        QueryResource {
            name: "assertion-status.rq",
            query: include_str!("../../resources/evidence/v1/queries/assertion-status.rq"),
        },
        QueryResource {
            name: "evidence-relations.rq",
            query: include_str!("../../resources/evidence/v1/queries/evidence-relations.rq"),
        },
        QueryResource {
            name: "source-lineage.rq",
            query: include_str!("../../resources/evidence/v1/queries/source-lineage.rq"),
        },
        QueryResource {
            name: "passage-selectors.rq",
            query: include_str!("../../resources/evidence/v1/queries/passage-selectors.rq"),
        },
        QueryResource {
            name: "stored-fact-references.rq",
            query: include_str!("../../resources/evidence/v1/queries/stored-fact-references.rq"),
        },
        QueryResource {
            name: "opposite-polarities.rq",
            query: include_str!("../../resources/evidence/v1/queries/opposite-polarities.rq"),
        },
        QueryResource {
            name: "unsupported-claims.rq",
            query: include_str!("../../resources/evidence/v1/queries/unsupported-claims.rq"),
        },
    ],
    compatibility_queries: &[
        QueryResource {
            name: "legacy-assertions.rq",
            query: include_str!("../../resources/evidence/v1/compatibility/legacy-assertions.rq"),
        },
        QueryResource {
            name: "legacy-source-spans.rq",
            query: include_str!("../../resources/evidence/v1/compatibility/legacy-source-spans.rq"),
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

pub mod propose;
pub mod reauth;
pub mod resolve;
pub mod review;

pub use propose::{
    AuthorityFence, Options as ProposeOptions, OrdinaryChanges, Proposed, ProposedRecord,
    operation_id, operation_iri, operation_recorded, propose, reconcile_operation_holds,
    reconcile_operation_holds_in,
};
pub use reauth::{
    MAX_REAUTHENTICATE_RECORDS, MAX_RECORD_TRIPLES, ReauthenticateOptions, ReauthenticateRecord,
    Reauthenticated, ReauthenticatedRecord, reauthenticate,
};
pub use review::claims::{
    self, ClaimOccurrence, ClaimOrigin, ClaimState, MAX_CLAIM_SCAN, MAX_CLAIMS, MEM_INSTRUCTIONS,
    PolicyClaims, PolicyRef, check_matching_policy, claim_states, recorded_settings,
    with_policy_claims,
};
pub use review::dependents::{
    self, Dependence, Dependent, Dependents, MAX_CHANGED, ReportDependent,
    dependents as find_dependents, dependents_at as find_dependents_at,
};
pub use review::identity::{
    self, EntityResolution, MAX_MEMBERS, MAX_RESOLVE, RESOLVES_TO, RebuiltMap, ResolvedEntity,
    rebuild as rebuild_resolution_map, resolve as resolve_entities,
    resolve_at as resolve_entities_at,
};
pub use review::{
    Changed, Decided, MapChange, Options as ReviewOptions, ProjectionChange, Reviewed, Withdrawn,
    WithdrawnClaim, review, withdraw,
};
pub use sparkles_core::evidence::review::{DecisionInput, Disposition, WithdrawalInput};
pub use sparkles_core::evidence::{
    ASSESSMENT_INPUT_KINDS, MAX_ASSESSMENT_INPUTS, MAX_CLAIMS as MAX_BATCH_CLAIMS, MAX_IDENTITIES,
    MAX_IDENTITY_ENTITIES, MIN_IDENTITY_ENTITIES, prepare_input,
};
pub use sparkles_core::evidence::{
    AssessmentInput, ClaimInput, Confidence, ConfidenceFacets, EvidenceInput, EvidenceLinkInput,
    EvidenceRefInput, EvidenceRefTarget, EvidenceRelation, IdentityNamespace, JSON_LEXICAL_UTF8_V1,
    JSON_LEXICAL_UTF8_VERSION, MethodRef, PassageInput, Polarity, RecordKey, RecordRef,
    RecordVersion, RenditionContent, RenditionInput, Scope, SourceContent, SourceLineage,
    SourceMetadata, SourceRevisionInput, SourceTimes,
};
pub use sparkles_core::evidence::{
    BoundaryInput, FoldChoice, NormalizationInput, NormalizationStatus, ValidityInput,
};

// ------------------------------------------------------------ settings ------------

pub use sparkles_core::evidence::{
    CanonicalSettings, ClaimKind, ConflictMode, IdentityInput, IdentityRelation,
    MAX_POLICY_NAME_BYTES, MatchingPolicy, Policy, RecordKind, Settings, Sha256Digest,
};

use crate::managed::{OwnerEntry, OwnerKind};
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
    settings_with_overrides(dataset).map(|(status, _)| status)
}

/// The recorded evidence settings and their sparse runtime layer from one registry.
pub fn settings_with_overrides(
    dataset: &Dataset,
) -> Result<(SettingsStatus, Option<crate::managed::EvidenceOverrides>)> {
    let snapshot = dataset.store().managed_snapshot()?;
    if let Some(reason) = snapshot.as_ref().and_then(|s| s.unavailable.as_ref()) {
        return Err(unavailable(reason.clone()));
    }
    let bytes = snapshot
        .as_ref()
        .and_then(|s| s.settings.get(&OwnerKind::Evidence).cloned());
    let status = status_from(snapshot.as_ref().map(|s| &s.graphs), bytes.as_deref())?;
    let overrides = snapshot
        .and_then(|mut s| s.graphs.owners.remove(&OwnerKind::Evidence))
        .and_then(|entry| entry.evidence_overrides);
    Ok((status, overrides))
}

fn unavailable(reason: String) -> Error {
    crate::managed::ManagedError::error(crate::managed::ManagedErrorKind::Unavailable, reason)
}

/// The recorded settings `bytes`, which must be readable by this build.
pub(crate) fn recorded(bytes: &[u8]) -> Result<Settings> {
    Settings::from_slice(bytes, &Control::none()).map_err(|e| {
        unavailable(format!(
            "{} cannot be read by this build: {e}",
            OwnerKind::Evidence.settings_file()
        ))
    })
}

/// The status of one registry, `graphs`, and the evidence settings `bytes` it records.
fn status_from(
    graphs: Option<&crate::managed::ManagedGraphs>,
    bytes: Option<&[u8]>,
) -> Result<SettingsStatus> {
    let revision = graphs.map_or(0, |g| g.revision);
    let Some(bytes) = bytes else {
        return Ok(SettingsStatus {
            settings: Settings::default(),
            digest: None,
            revision,
            managed: Vec::new(),
            detached: Vec::new(),
        });
    };
    let protected: &[String] = graphs
        .and_then(|g| g.owners.get(&OwnerKind::Evidence))
        .map_or(&[], |e| &e.graphs);
    let settings = recorded(bytes)?;
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
        digest: Some(crate::managed::settings_digest(bytes)),
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
/// writes nothing. A registry whose `evidence.json` does not match the digest that
/// `managed.json` records, as after a crash between the two files, is checked against
/// the protection `managed.json` records and repaired by the configuration.
///
/// Placing a graph under management does not authenticate what it already holds.
/// Typed evidence operations trust a record only through its verified publication.
pub fn configure(
    dataset: &Dataset,
    settings: &Settings,
    control: &Control,
) -> Result<SettingsStatus> {
    configure_recorded(dataset, settings, OverrideUpdate::Preserve, None, control)
}

/// Record settings and replace their sparse runtime layer in the same publication.
/// `None` for `overrides` means no declaration applies. An empty declared layer can
/// reconcile an unconfigured disabled template without recording any settings.
pub fn configure_layered(
    dataset: &Dataset,
    settings: &Settings,
    overrides: Option<serde_json::Map<String, serde_json::Value>>,
    expected: Option<u64>,
    control: &Control,
) -> Result<SettingsStatus> {
    configure_recorded(
        dataset,
        settings,
        OverrideUpdate::Replace(overrides),
        expected,
        control,
    )
}

enum OverrideUpdate {
    Preserve,
    Replace(Option<serde_json::Map<String, serde_json::Value>>),
}

fn configure_recorded(
    dataset: &Dataset,
    settings: &Settings,
    overrides: OverrideUpdate,
    expected: Option<u64>,
    control: &Control,
) -> Result<SettingsStatus> {
    let store = dataset.store();
    let canonical = settings.canonical(control).map_err(unsupported_profile)?;
    // A registry whose settings file does not match, as after a crash between the
    // settings file and `managed.json`, is checked against what `managed.json`
    // records and repaired by this configuration.
    let snapshot = store.managed_snapshot()?;
    let revision = snapshot.as_ref().map_or(0, |s| s.graphs.revision);
    if let Some(expected) = expected
        && revision != expected
    {
        return Err(Error::Conflict(format!(
            "the managed-graph registry is at revision {revision}, not {expected}; read it again"
        )));
    }
    let current = snapshot
        .as_ref()
        .and_then(|s| s.graphs.owners.get(&OwnerKind::Evidence));
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
        let allowed_noop = match &overrides {
            OverrideUpdate::Replace(Some(patch)) => patch.is_empty(),
            OverrideUpdate::Preserve | OverrideUpdate::Replace(None) => {
                canonical.bytes() == Settings::default().canonical(control)?.bytes()
            }
        };
        if !allowed_noop {
            return Err(Error::invalid(
                "the evidence profile is not enabled; enable it before storing disabled settings",
            ));
        }
        if let Some(reason) = snapshot.as_ref().and_then(|s| s.unavailable.as_ref()) {
            return Err(unavailable(reason.clone()));
        }
        let bytes = snapshot
            .as_ref()
            .and_then(|s| s.settings.get(&OwnerKind::Evidence).cloned());
        return status_from(snapshot.as_ref().map(|s| &s.graphs), bytes.as_deref());
    }
    control.check()?;
    let mut entry = OwnerEntry::for_settings(settings.profile, roles, canonical.bytes());
    entry.evidence_overrides = match overrides {
        OverrideUpdate::Preserve => current.and_then(|entry| entry.evidence_overrides.clone()),
        OverrideUpdate::Replace(patch) => patch.and_then(crate::managed::EvidenceOverrides::new),
    };
    let graphs =
        store.enable_managed_at(OwnerKind::Evidence, entry, canonical.bytes(), revision)?;
    status_from(Some(&graphs), Some(canonical.bytes()))
}

/// Stop protecting `graph`, a graph of the evidence settings' roles, so that ordinary
/// writes can change it. The settings keep the graph in its roles, and it is protected
/// again by the next [`configure`]. Records written to the graph while it is detached
/// are not authenticated by protecting it again.
pub fn detach(dataset: &Dataset, graph: &str) -> Result<SettingsStatus> {
    let store = dataset.store();
    let Some(snapshot) = store.managed_snapshot()? else {
        return Err(crate::managed::ManagedError::error(
            crate::managed::ManagedErrorKind::UnsupportedProfile,
            "the evidence profile does not manage graphs in this dataset",
        ));
    };
    if let Some(reason) = snapshot.unavailable {
        return Err(unavailable(reason));
    }
    // The recorded settings must be readable before protection is removed, and the
    // revision check keeps them the settings of the registry that is changed.
    let bytes = snapshot.settings.get(&OwnerKind::Evidence).cloned();
    if let Some(bytes) = &bytes {
        recorded(bytes)?;
    }
    let graphs = store.detach_managed_at(OwnerKind::Evidence, graph, snapshot.graphs.revision)?;
    status_from(Some(&graphs), bytes.as_deref())
}

// --------------------------------------------------------------- holds ------------

pub use sparkles_core::branch::CommitRef;
pub use sparkles_core::holds::{DEFAULT_GRACE_MS, MAX_COMMITS, SourceHold, SourceHoldInfo};
pub use sparkles_core::store::Acquired;

/// The durable source holds of the evidence profile in `dataset` and its branches, by
/// id. Fails with `holds-unavailable` when the dataset's `holds.json` cannot be read.
pub fn holds(dataset: &Dataset) -> Result<Vec<SourceHold>> {
    Ok(dataset
        .store()
        .source_holds()?
        .into_iter()
        .filter(|h| h.owner == OwnerKind::Evidence)
        .collect())
}

/// The evidence holds with their originating dataset IDs when foreign.
pub fn hold_details(dataset: &Dataset) -> Result<Vec<SourceHoldInfo>> {
    let mut holds = dataset.store().source_hold_details()?;
    holds.retain(|h| h.hold.owner == OwnerKind::Evidence);
    Ok(holds)
}

/// Keep `commits` readable for the evidence profile under the hold `id`. Each commit
/// names `main` or a listed branch by its id, as an evidence reference does. The hold
/// is durable when this returns: no history trimming, compaction or generation
/// retirement removes the commits' states, and a branch the hold names is deleted only
/// by a deletion that releases its holds. Acquiring the same hold again returns it
/// unchanged. A hold of that id with other commits is `hold-conflict`, more than
/// [`MAX_COMMITS`] held commits in the dataset is `holds-limit`, and a commit that is
/// no longer readable is `history-gone`, and these refusals record nothing. An I/O error
/// while the holds file is replaced may leave the hold recorded and protecting its
/// commits, so repeat the acquisition, release it or reconcile it. The first hold raises
/// the dataset reader to 7.
pub fn acquire_hold(dataset: &Dataset, id: uuid::Uuid, commits: &[CommitRef]) -> Result<Acquired> {
    acquire_hold_for(dataset, id, commits, None)
}

/// [`acquire_hold`] for an operation in `for_dataset`, when it differs from `dataset`.
/// A `for_dataset` equal to `dataset`'s id is normalized to `None`. Any hold naming
/// another dataset writes `holds.json` as format 2.
pub fn acquire_hold_for(
    dataset: &Dataset,
    id: uuid::Uuid,
    commits: &[CommitRef],
    for_dataset: Option<uuid::Uuid>,
) -> Result<Acquired> {
    dataset
        .store()
        .acquire_hold_for(OwnerKind::Evidence, id, commits, for_dataset)
}

/// Release the evidence hold `id`, returning whether it existed. Its commits can be
/// collected once `holds.json` is durable without it.
pub fn release_hold(dataset: &Dataset, id: uuid::Uuid) -> Result<bool> {
    Ok(!dataset
        .store()
        .release_holds(OwnerKind::Evidence, &[id])?
        .is_empty())
}

/// Release local evidence holds acquired at least `grace_ms` milliseconds ago that
/// `keep` does not keep. Foreign holds stay in place and are never passed to the
/// callback. Use [`reconcile_operation_holds_in`] to verify and release foreign holds.
/// Returns the released ids.
pub fn reconcile_holds(
    dataset: &Dataset,
    grace_ms: i64,
    keep: &mut dyn FnMut(&SourceHold) -> Result<bool>,
) -> Result<Vec<uuid::Uuid>> {
    dataset
        .store()
        .reconcile_holds(OwnerKind::Evidence, grace_ms, keep)
}
