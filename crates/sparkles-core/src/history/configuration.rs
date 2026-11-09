//! Configuration identities are provenance, not credentials for selecting a view.

use crate::evidence::Sha256Digest;
use serde::{Deserialize, Serialize};

/// The complete immutable managed configuration associated with a selected view.
///
/// Deserializing an identity does not authenticate it. The store must verify its
/// published lineage and exact data binding before returning an evidence view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigurationIdentity {
    pub(crate) dataset_id: uuid::Uuid,
    pub(crate) lineage_id: uuid::Uuid,
    pub(crate) generation: Sha256Digest,
}

impl ConfigurationIdentity {
    /// The logical dataset family that published the configuration.
    pub fn dataset_id(&self) -> uuid::Uuid {
        self.dataset_id
    }

    /// The independently activated configuration-history lineage.
    pub fn lineage_id(&self) -> uuid::Uuid {
        self.lineage_id
    }

    /// SHA-256 of the exact canonical generation document.
    pub fn generation(&self) -> Sha256Digest {
        self.generation
    }
}

/// The primary binding of an RDF revision or a durable named view.
///
/// Legacy absence is explicit and never filled from current settings. An imported
/// binding records its original identity without certifying it as native history
/// of a reidentified destination.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ConfigurationBinding {
    /// The revision predates authenticated configuration coverage.
    #[default]
    LegacyUnbound,
    /// A complete generation authenticated by the store's primary publication.
    Known {
        configuration: ConfigurationIdentity,
    },
    /// Source provenance retained by a clone or restore under a new identity.
    ImportedUnbound { source: ConfigurationIdentity },
}

impl ConfigurationBinding {
    /// The native generation, when this binding authenticates one.
    pub fn configuration(&self) -> Option<ConfigurationIdentity> {
        match self {
            Self::Known { configuration } => Some(*configuration),
            Self::LegacyUnbound | Self::ImportedUnbound { .. } => None,
        }
    }

    pub(crate) fn validate(&self, family: uuid::Uuid) -> crate::Result<()> {
        let identity = match self {
            Self::LegacyUnbound => return Ok(()),
            Self::Known { configuration } => {
                if configuration.dataset_id != family {
                    return Err(crate::Error::Corrupt(
                        "foreign native configuration binding".into(),
                    ));
                }
                configuration
            }
            Self::ImportedUnbound { source } => source,
        };
        if identity.dataset_id.is_nil() || identity.lineage_id.is_nil() {
            return Err(crate::Error::Corrupt(
                "incomplete configuration identity".into(),
            ));
        }
        Ok(())
    }
}

/// Exact selector provenance in the new primary metadata. A timestamp remains
/// the original signed integer, rather than a potentially lossy formatted date.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum ConfigurationSelector {
    /// A pre-activation pin did not persist whether it selected Head or a commit.
    /// This is absence of provenance, never reconstructed as an old selector.
    LegacyUnspecified,
    Head,
    Commit {
        seq: u64,
    },
    Time {
        unix_ms: i64,
    },
    Snapshot {
        name: String,
    },
}

impl ConfigurationSelector {
    /// The caller must reserve the bounded optional snapshot-name copy.
    pub(crate) fn to_at(&self) -> Option<super::At> {
        Some(match self {
            Self::LegacyUnspecified => return None,
            Self::Head => super::At::Head,
            Self::Commit { seq } => super::At::Commit(*seq),
            Self::Time { unix_ms } => super::At::Time(*unix_ms),
            Self::Snapshot { name } => super::At::Snapshot(name.clone()),
        })
    }

    /// Call only after the caller has reserved the bounded selector copy.
    pub(crate) fn capture(at: &super::At) -> Self {
        match at {
            super::At::Head => Self::Head,
            super::At::Commit(seq) => Self::Commit { seq: *seq },
            super::At::Time(unix_ms) => Self::Time { unix_ms: *unix_ms },
            super::At::Snapshot(name) => Self::Snapshot { name: name.clone() },
        }
    }

    pub(crate) fn validate(&self) -> crate::Result<()> {
        if let Self::Snapshot { name } = self
            && !super::valid_name(name)
        {
            return Err(crate::Error::Corrupt(
                "invalid configuration snapshot selector".into(),
            ));
        }
        Ok(())
    }
}

/// A child's own immutable root semantics, independent of the ancestor that
/// physically serves its RDF root after retention or compaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct BranchConfigurationOrigin {
    pub(crate) source_branch: uuid::Uuid,
    pub(crate) selector: ConfigurationSelector,
    pub(crate) binding: ConfigurationBinding,
    pub(crate) first_bound_own: u64,
}

impl BranchConfigurationOrigin {
    pub(crate) fn validate(&self, family: uuid::Uuid, root_seq: u64) -> crate::Result<()> {
        if self.source_branch.is_nil()
            || root_seq.checked_add(1) != Some(self.first_bound_own)
            || matches!(self.selector, ConfigurationSelector::LegacyUnspecified)
        {
            return Err(crate::Error::Corrupt(
                "invalid branch configuration boundary".into(),
            ));
        }
        self.selector.validate()?;
        self.binding.validate(family)
    }
}

/// A durable named view's semantic selection. Legacy pins explicitly lack their
/// old selector provenance; no current configuration is substituted for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PinnedConfiguration {
    pub(crate) binding: ConfigurationBinding,
    pub(crate) selector: ConfigurationSelector,
}

impl PinnedConfiguration {
    pub(crate) fn legacy() -> Self {
        Self {
            binding: ConfigurationBinding::LegacyUnbound,
            selector: ConfigurationSelector::LegacyUnspecified,
        }
    }

    pub(crate) fn validate(&self, family: uuid::Uuid) -> crate::Result<()> {
        self.binding.validate(family)?;
        self.selector.validate()?;
        if self.binding.configuration().is_some()
            && matches!(self.selector, ConfigurationSelector::LegacyUnspecified)
        {
            return Err(crate::Error::Corrupt(
                "a native named view has no selector provenance".into(),
            ));
        }
        Ok(())
    }
}
