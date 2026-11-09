//! Destination publication for origin2. This is syntax plus publication checks,
//! not a free constructor for an imported Context, view or replay exemption.
use super::*;
use crate::managed::ManagedState;
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Publication {
    /// The immutable destination genesis, independent of later settings-only
    /// publications at the same data sequence.
    pub(super) genesis: ConfigurationIdentity,
    pub(super) coverage: Sha256Digest,
    /// Canonical semantic creation commit, encoded by the existing generation
    /// codec with origin=create. Compaction may change its physical location and
    /// origin string without changing this original complete CommitInfo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) creation_commit: Option<Sha256Digest>,
}

impl Publication {
    pub(super) fn validate(&self, family: Uuid) -> Result<()> {
        ConfigurationBinding::Known {
            configuration: self.genesis,
        }
        .validate(family)
    }

    /// Only the actual Family opening supplies installed current state. A caller
    /// cannot substitute an orphan root as the publication from which to walk.
    pub(super) fn authenticate(
        &self,
        root: &Path,
        family: &crate::store::family::Family,
        lock: &Arc<crate::store::ProcessLock>,
        preparation: &Preparation,
    ) -> Result<()> {
        family.verify_origin_lock(root, lock)?;
        let directory = documents::Opened::root(root, preparation)?;
        let bytes = directory
            .child("dataset.json", false, preparation)?
            .bytes(None, preparation)?;
        let actual_id = documents::dataset_id(&bytes.value, preparation)?;
        self.validate(actual_id)?;
        drop(bytes);
        let state = family.managed.load_full();
        let installed = match state.as_deref() {
            Some(ManagedState::Installed(installed)) => installed,
            _ => {
                return Err(managed_history::unavailable(
                    "destination publication has no actual installed configuration",
                ));
            }
        };
        let history = installed.configuration_history.as_ref().ok_or_else(|| {
            managed_history::unavailable("destination publication is not activated")
        })?;
        if history.lineage_id != self.genesis.lineage_id || history.coverage != self.coverage {
            return Err(managed_history::unavailable(
                "destination origin belongs to another installed lineage or cutoff",
            ));
        }
        let coverage = managed_history::read_coverage(root, actual_id, history, preparation)?;
        if coverage.value.activation_generation != self.genesis.generation {
            return Err(managed_history::unavailable(
                "destination genesis differs from its actual activation",
            ));
        }
        drop(coverage);
        let current = managed_history::resolve(
            root,
            actual_id,
            history,
            history.identity(actual_id),
            preparation,
        )?;
        if !current.value.matches_installed(installed) {
            return Err(managed_history::unavailable(
                "destination current settings differ from their publication",
            ));
        }
        drop(current);
        let genesis =
            managed_history::resolve(root, actual_id, history, self.genesis, preparation)?;
        if genesis.value.ordinal != 1 || genesis.value.previous.is_some() {
            return Err(managed_history::unavailable(
                "destination origin does not name its genuine genesis",
            ));
        }
        drop(genesis);
        if !family.managed.load_full().as_ref().is_some_and(|actual| {
            state
                .as_ref()
                .is_some_and(|selected| Arc::ptr_eq(actual, selected))
        }) {
            return Err(Error::Conflict(
                "destination configuration changed during origin verification".into(),
            ));
        }
        family.verify_origin_lock(root, lock)?;
        preparation.check()
    }

    /// Only a real new creation root receives this precise native exception.
    /// Restored old root0 and all later bases continue through ordinary binding
    /// or exact imported-base validation. The complete original semantic commit
    /// remains bound after compaction moves its physical generation.
    pub(super) fn creation_origin(
        &self,
        actual: &crate::commit::RetainedGenerationCommit,
        preparation: &Preparation,
    ) -> Result<Option<(u64, ConfigurationBinding)>> {
        preparation.check()?;
        if actual.commit.seq != 0 || self.creation_commit.is_none() {
            return Ok(None);
        }
        let binding = ConfigurationBinding::Known {
            configuration: self.genesis,
        };
        if actual.dataset_id != self.genesis.dataset_id
            || actual.configuration != binding
            || actual.commit.kind != crate::commit::CommitKind::Create
            || actual.commit.generation != 1
        {
            return Err(managed_history::unavailable(
                "destination creation root differs from its actual primary commit",
            ));
        }
        let digest = creation_digest(actual.dataset_id, &actual.commit, binding, preparation)?;
        if Some(digest) != self.creation_commit {
            return Err(managed_history::unavailable(
                "destination creation commit differs from its original semantic pin",
            ));
        }
        preparation.check()?;
        Ok(Some((0, binding)))
    }
}

/// Every input of the established primary encoder here has a fixed maximum
/// representation (UUID, integer, enum and fixed timestamp), with no supplied
/// string/map/payload. Admit its bounded temporary Vec before construction.
pub(super) fn creation_digest(
    family: Uuid,
    commit: &CommitInfo,
    binding: ConfigurationBinding,
    preparation: &Preparation,
) -> Result<Sha256Digest> {
    let _temporary = preparation.reserve(16 << 10)?;
    preparation.check()?;
    let bytes = crate::commit::gen_commit_bytes_with_binding(family, "create", commit, binding);
    if bytes.capacity() > 16 << 10 {
        return Err(preparation.exhausted());
    }
    Sha256Digest::of_bytes(&bytes, preparation.control())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preparation() -> Preparation {
        Preparation::new(crate::task::Control::none(), 16 << 20).unwrap()
    }
    fn identity() -> ConfigurationIdentity {
        ConfigurationIdentity {
            dataset_id: Uuid::from_u128(1),
            lineage_id: Uuid::from_u128(2),
            generation: Sha256Digest::from_digest_bytes([3; 32]),
        }
    }
    fn origin() -> Origin {
        Origin {
            format: 2,
            dataset_id: identity().dataset_id,
            destination: Some(Publication {
                genesis: identity(),
                coverage: Sha256Digest::from_digest_bytes([4; 32]),
                creation_commit: None,
            }),
            sources: vec![Sha256Digest::from_digest_bytes([5; 32])],
            bases: Vec::new(),
            ranges: Vec::new(),
        }
    }

    #[test]
    fn destination_origin_codec_has_an_explicit_versioned_anchor_and_preserves_v1() {
        let preparation = preparation();
        let before = preparation.room();
        let value = origin();
        let bytes = managed_history::encode(&value, &preparation).unwrap();
        let decoded = Origin::decode(&bytes.value, &preparation).unwrap();
        assert_eq!(decoded.value.format, 2);
        assert_eq!(
            decoded.value.destination.as_ref().unwrap().genesis,
            identity()
        );
        drop(decoded);
        let mut legacy = value;
        legacy.format = 1;
        legacy.destination = None;
        let legacy = managed_history::encode(&legacy, &preparation).unwrap();
        assert!(
            !std::str::from_utf8(&legacy.value)
                .unwrap()
                .contains("destination")
        );
        drop(Origin::decode(&legacy.value, &preparation).unwrap());
        for (format, destination) in [(1, true), (2, false), (3, false)] {
            let mut candidate = origin();
            candidate.format = format;
            if !destination {
                candidate.destination = None;
            }
            let wire = managed_history::encode(&candidate, &preparation).unwrap();
            assert!(Origin::decode(&wire.value, &preparation).is_err());
        }
        let mut foreign = origin();
        foreign.destination.as_mut().unwrap().genesis.dataset_id = Uuid::new_v4();
        let foreign = managed_history::encode(&foreign, &preparation).unwrap();
        assert!(Origin::decode(&foreign.value, &preparation).is_err());
        drop(foreign);
        drop(bytes);
        drop(legacy);
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn destination_creation_pin_covers_the_full_commit_and_survives_physical_relocation() {
        let preparation = preparation();
        let before = preparation.room();
        let binding = ConfigurationBinding::Known {
            configuration: identity(),
        };
        let commit = CommitInfo {
            seq: 0,
            timestamp_ms: 100,
            kind: crate::commit::CommitKind::Create,
            inserted: 7,
            deleted: 0,
            quads: 7,
            generation: 1,
            bulk: true,
            exact: true,
            reconstructed: false,
            default_graph: true,
            unvalidated: false,
        };
        let publication = Publication {
            genesis: identity(),
            coverage: Sha256Digest::from_digest_bytes([4; 32]),
            creation_commit: Some(
                creation_digest(identity().dataset_id, &commit, binding, &preparation).unwrap(),
            ),
        };
        let actual = |commit, configuration| crate::commit::RetainedGenerationCommit {
            dataset_id: identity().dataset_id,
            commit,
            configuration,
            origin: "compaction".to_owned(),
            wal_strict_from: Some(0),
        };
        assert_eq!(
            publication
                .creation_origin(&actual(commit, binding), &preparation)
                .unwrap(),
            Some((0, binding))
        );
        for changed in [
            CommitInfo {
                inserted: 8,
                ..commit
            },
            CommitInfo { quads: 8, ..commit },
            CommitInfo {
                timestamp_ms: 101,
                ..commit
            },
            CommitInfo {
                bulk: false,
                ..commit
            },
            CommitInfo {
                unvalidated: true,
                ..commit
            },
            CommitInfo {
                exact: false,
                ..commit
            },
            CommitInfo {
                default_graph: false,
                ..commit
            },
            CommitInfo {
                reconstructed: true,
                ..commit
            },
            CommitInfo {
                generation: 2,
                ..commit
            },
        ] {
            assert!(
                publication
                    .creation_origin(&actual(changed, binding), &preparation)
                    .is_err()
            );
        }
        assert!(
            publication
                .creation_origin(
                    &actual(
                        commit,
                        ConfigurationBinding::ImportedUnbound { source: identity() }
                    ),
                    &preparation
                )
                .is_err()
        );
        assert!(
            publication
                .creation_origin(
                    &actual(CommitInfo { seq: 1, ..commit }, binding),
                    &preparation
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(preparation.room(), before);
    }

    #[test]
    fn destination_codec_and_creation_temporaries_keep_receiving_controls() {
        let preparation = preparation();
        let encoded = managed_history::encode(&origin(), &preparation).unwrap();
        let control = crate::task::Control::none();
        let cancelled = Preparation::new(control.clone(), 16 << 20).unwrap();
        let room = cancelled.room();
        control.cancel.cancel();
        assert!(matches!(
            Origin::decode(&encoded.value, &cancelled),
            Err(Error::Cancelled)
        ));
        assert_eq!(cancelled.room(), room);
        let small = Preparation::new(crate::task::Control::none(), 4096).unwrap();
        let room = small.room();
        assert!(matches!(
            Origin::decode(&encoded.value, &small),
            Err(Error::BudgetExceeded(_))
        ));
        assert_eq!(small.room(), room);
        // Pure codec controls do not claim genuine destination publication or
        // imported physical authority; authenticate requires the real actor.
    }
}
