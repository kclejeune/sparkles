//! Store-minted RDF/configuration pairs. Deserialized provenance and caller-owned
//! snapshots never enter this constructor path.
use super::*;
use crate::access::{Preparation, Reservation};
use crate::history::{At, ConfigurationBinding, ConfigurationIdentity, HistoryOptions, ReadLimit};
use crate::managed::{ManagedState, OwnerEntry, OwnerKind};

/// An exact RDF view and its authenticated complete semantic configuration.
/// Clones retain the same payload owners and preparation until the last drop.
/// Current authorization and source availability must still be checked on use.
///
/// ```compile_fail,E0451
/// use sparkles_core::store::BoundEvidenceView;
/// fn forge() -> BoundEvidenceView { BoundEvidenceView { inner: todo!() } }
/// ```
#[derive(Clone)]
pub struct BoundEvidenceView {
    inner: Arc<BoundInner>,
}

struct BoundInner {
    family: std::sync::Weak<family::Family>,
    dataset_id: uuid::Uuid,
    logical_branch: uuid::Uuid,
    serving_branch: uuid::Uuid,
    snapshot: Arc<Snapshot>,
    commit: CommitInfo,
    requested: At,
    selected_by: At,
    configuration: ConfigurationIdentity,
    original_data_binding: ConfigurationBinding,
    document: managed_history::Owned<managed_history::Generation>,
    _held: Reservation,
}

impl std::fmt::Debug for BoundEvidenceView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundEvidenceView")
            .field("dataset_id", &self.dataset_id())
            .field("logical_branch", &self.logical_branch_id())
            .field("serving_branch", &self.serving_branch_id())
            .field("commit", &self.commit().seq)
            .field("configuration", &self.configuration())
            .finish_non_exhaustive()
    }
}

impl BoundEvidenceView {
    pub fn dataset_id(&self) -> uuid::Uuid {
        self.inner.dataset_id
    }
    pub fn logical_branch_id(&self) -> uuid::Uuid {
        self.inner.logical_branch
    }
    pub fn serving_branch_id(&self) -> uuid::Uuid {
        self.inner.serving_branch
    }
    pub fn commit(&self) -> CommitInfo {
        self.inner.commit
    }
    pub fn requested_at(&self) -> &At {
        &self.inner.requested
    }
    pub fn selected_by(&self) -> &At {
        &self.inner.selected_by
    }
    pub fn configuration(&self) -> ConfigurationIdentity {
        self.inner.configuration
    }
    pub fn original_data_binding(&self) -> ConfigurationBinding {
        self.inner.original_data_binding
    }

    /// Physical RDF only. Apply the caller's current mask as well. As with a
    /// PreparedView, independently cloning raw data requires caller accounting;
    /// a raw clone carries no authority to construct another BoundEvidenceView.
    pub fn snapshot(&self) -> &Snapshot {
        &self.inner.snapshot
    }

    /// Exact original owner bytes. These describe history and grant no access.
    pub fn owner_settings(&self, owner: OwnerKind) -> Option<&[u8]> {
        self.inner
            .document
            .value
            .owner_settings
            .get(&owner)
            .map(|value| value.as_bytes())
    }

    pub fn owner(&self, owner: OwnerKind) -> Option<&OwnerEntry> {
        self.inner.document.value.owners.get(&owner)
    }

    /// A reopened or replaced dataset with the same UUID is a different runtime
    /// owner. This check is independent of current authorization and protections.
    pub fn validate_instance(&self, store: &Store) -> Result<()> {
        if self.dataset_id() != store.owner_dataset_id()
            || self.logical_branch_id() != store.branch_id()
            || !self.inner.family.ptr_eq(&Arc::downgrade(&store.family))
        {
            return Err(managed_history::unavailable(
                "evidence view belongs to another dataset instance",
            ));
        }
        Ok(())
    }
}

/// Private selection also represents a genuinely unbound historical fork. Only
/// the public evidence constructor requires a native semantic generation.
pub(super) struct SelectedPair {
    pub(super) snapshot: Arc<Snapshot>,
    pub(super) commit: CommitInfo,
    pub(super) requested: At,
    pub(super) selected_by: At,
    pub(super) logical_branch: uuid::Uuid,
    pub(super) serving_branch: uuid::Uuid,
    pub(super) semantic: ConfigurationBinding,
    pub(super) data: ConfigurationBinding,
    state: Arc<ManagedState>,
    held: Reservation,
}

impl SelectedPair {
    pub(super) fn branch_origin(&self) -> Result<crate::history::BranchConfigurationOrigin> {
        Ok(crate::history::BranchConfigurationOrigin {
            source_branch: self.logical_branch,
            selector: crate::history::ConfigurationSelector::capture(&self.requested),
            binding: self.semantic,
            first_bound_own: self
                .commit
                .seq
                .checked_add(1)
                .ok_or_else(|| Error::invalid("branch configuration boundary overflow"))?,
        })
    }
}

impl Store {
    /// Pin the exact pair selected by a Store-minted view. Unlike the legacy
    /// sequence-only API, an existing name at the same RDF commit conflicts when
    /// its semantic generation differs.
    pub fn create_bound_snapshot(
        &self,
        name: &str,
        view: &BoundEvidenceView,
        options: &crate::history::SnapshotOptions,
    ) -> Result<(crate::history::NamedSnapshot, bool)> {
        view.validate_instance(self)?;
        let configuration = crate::history::PinnedConfiguration {
            binding: managed_history::binding(view.configuration()),
            selector: crate::history::ConfigurationSelector::capture(view.requested_at()),
        };
        let result = self.create_snapshot_selected(
            name,
            &At::Commit(view.commit().seq),
            options,
            Some(&configuration),
        )?;
        if options.warm && result.1 {
            self.warm_snapshots();
        }
        Ok(result)
    }

    pub(super) fn select_branch_configuration(
        &self,
        store: &Store,
        at: &At,
        preparation: &Preparation,
    ) -> Result<Option<SelectedPair>> {
        match self.managed_state_shared().as_deref() {
            None => return Ok(None),
            Some(ManagedState::Installed(installed))
                if installed.configuration_history.is_none() =>
            {
                return Ok(None);
            }
            Some(ManagedState::Unavailable { reason, .. }) => {
                return Err(managed_history::unavailable(reason.clone()));
            }
            Some(ManagedState::Installed(_)) => {}
        }
        self.select_configuration_pair(store, at, preparation)
            .map(Some)
    }

    /// Select an evidence view on main. Use evidence_view_with_branch on the
    /// owning main Store for a retained branch, including inherited history.
    pub fn evidence_view(&self, at: &At, preparation: &Preparation) -> Result<BoundEvidenceView> {
        self.evidence_view_with_branch(self, at, preparation)
    }

    /// Select through the family's read-only retained-origin path. The logical
    /// branch root's semantics are resolved before physical ancestor delegation.
    pub fn evidence_view_with_branch(
        &self,
        store: &Store,
        at: &At,
        preparation: &Preparation,
    ) -> Result<BoundEvidenceView> {
        let pair = self.select_configuration_pair(store, at, preparation)?;
        let configuration = pair.semantic.configuration().ok_or_else(|| {
            managed_history::unavailable("historical evidence configuration is explicitly unbound")
        })?;
        let installed = match pair.state.as_ref() {
            ManagedState::Installed(installed) => installed,
            ManagedState::Unavailable { .. } => unreachable!("selection checked installation"),
        };
        let history = installed.configuration_history.as_ref().ok_or_else(|| {
            managed_history::unavailable("configuration history is not activated")
        })?;
        let document = if let Some(root) = self.managed_root() {
            managed_history::resolve(
                &root,
                self.owner_dataset_id(),
                history,
                configuration,
                preparation,
            )?
        } else {
            let published = installed.history_memory.as_ref().ok_or_else(|| {
                managed_history::unavailable("configuration memory history is missing")
            })?;
            if published.prepared.identity != history.identity(self.owner_dataset_id()) {
                return Err(managed_history::unavailable(
                    "configuration memory root differs",
                ));
            }
            managed_history::resolve_memory(published, configuration, preparation)?
        };
        preparation.check()?;
        Ok(BoundEvidenceView {
            inner: Arc::new(BoundInner {
                family: Arc::downgrade(&self.family),
                dataset_id: self.owner_dataset_id(),
                logical_branch: pair.logical_branch,
                serving_branch: pair.serving_branch,
                snapshot: pair.snapshot,
                commit: pair.commit,
                requested: pair.requested,
                selected_by: pair.selected_by,
                configuration,
                original_data_binding: pair.data,
                document,
                _held: pair.held,
            }),
        })
    }

    pub(super) fn select_configuration_pair(
        &self,
        store: &Store,
        at: &At,
        preparation: &Preparation,
    ) -> Result<SelectedPair> {
        preparation.check()?;
        if self.is_branch() || !Arc::ptr_eq(&self.family, &store.family) {
            return Err(Error::invalid(
                "evidence selection uses the actual owning main Store",
            ));
        }
        if let At::Snapshot(name) = at
            && !crate::history::valid_name(name)
        {
            return Err(Error::invalid("invalid evidence snapshot name"));
        }
        let metadata = preparation.reserve(1024)?;
        let before = self.managed_state_shared().ok_or_else(|| {
            managed_history::unavailable("configuration history is not activated")
        })?;
        let installed = match before.as_ref() {
            ManagedState::Installed(installed) => installed,
            ManagedState::Unavailable { reason, .. } => {
                return Err(managed_history::unavailable(reason.clone()));
            }
        };
        let current = installed
            .configuration_history
            .as_ref()
            .ok_or_else(|| managed_history::unavailable("configuration history is not activated"))?
            .identity(self.owner_dataset_id());
        let allowance = preparation.room();
        let mut held = preparation.reserve(allowance)?;
        let options = HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        let (snapshot, resolved, serving_branch) =
            self.snapshot_at_with_branch_limited(store, at, &options, allowance)?;
        let limit = ReadLimit {
            options: &options,
            memory: Some(allowance),
            held: 0,
        };
        let retained = snapshot
            .retained_preparation_bytes()
            .checked_add(snapshot.generation.retained_bytes_limited(&limit)?)
            .and_then(|bytes| bytes.checked_add(1024))
            .ok_or_else(|| preparation.exhausted())?;
        if retained > allowance {
            return Err(preparation.exhausted());
        }
        held.release_to(retained);
        held.merge(metadata);
        let data = self.configuration_of_branch(store, resolved.commit.seq, preparation)?;
        let mut selected_by = at.clone();
        let semantic = match at {
            At::Head => managed_history::binding(current),
            At::Commit(_) | At::Time(_) => data,
            At::Snapshot(name) => {
                let _copy = preparation.reserve(256)?;
                let pinned = if let Some(history) = &store.history {
                    let history = preparation.lock(history)?;
                    history
                        .pins
                        .get(name)
                        .map(|pin| (pin.seq, pin.expires_ms, pin.configuration.clone()))
                } else if let Some(history) = &store.mem_history {
                    let history = preparation.lock(history)?;
                    history
                        .pins
                        .get(name)
                        .map(|(pin, _)| (pin.seq, pin.expires_ms, pin.configuration.clone()))
                } else {
                    None
                }
                .ok_or_else(|| {
                    managed_history::unavailable("named evidence view is no longer retained")
                })?;
                if pinned.0 != resolved.commit.seq
                    || pinned.1.is_some_and(|expiry| expiry <= store.now_ms())
                {
                    return Err(managed_history::unavailable(
                        "named evidence view changed or expired",
                    ));
                }
                match pinned.2 {
                    Some(configuration) => {
                        configuration.validate(self.owner_dataset_id())?;
                        if let Some(origin) = configuration.selector.to_at() {
                            selected_by = origin;
                        }
                        configuration.binding
                    }
                    None => ConfigurationBinding::LegacyUnbound,
                }
            }
        };
        let after = self.managed_state_shared();
        if !after
            .as_ref()
            .is_some_and(|after| Arc::ptr_eq(after, &before))
        {
            // A transient conflict is explicit. No unbounded retry is possible
            // when an embedding caller supplies no deadline.
            return Err(managed_history::unavailable(
                "configuration changed during evidence selection",
            ));
        }
        preparation.check()?;
        Ok(SelectedPair {
            snapshot,
            commit: resolved.commit,
            requested: at.clone(),
            selected_by,
            logical_branch: store.branch_id(),
            serving_branch,
            semantic,
            data,
            state: before,
            held,
        })
    }

    fn configuration_of_branch(
        &self,
        store: &Store,
        seq: u64,
        preparation: &Preparation,
    ) -> Result<ConfigurationBinding> {
        enum Source<'a> {
            Borrowed(&'a Store),
            Resident(Arc<Store>),
            Cold(uuid::Uuid),
        }
        use branching::readonly::{OriginRead, Purpose};
        let mut source = Source::Borrowed(store);
        for _ in 0..1024 {
            preparation.check()?;
            let resident = match &source {
                Source::Borrowed(store) => Some(*store),
                Source::Resident(store) => Some(store.as_ref()),
                Source::Cold(_) => None,
            };
            if let Some(store) = resident {
                if let Some(identity) = &store.branching.ident
                    && seq < identity.from.seq
                {
                    // Do not normalize through intermediate branch roots: each
                    // one's own primary origin must win at its exact sequence.
                    source = Source::Cold(identity.from.branch_id);
                    continue;
                }
                return store.configuration_binding_at(seq, preparation);
            }
            let Source::Cold(id) = source else {
                unreachable!()
            };
            if id == self.branch_id() {
                source = Source::Borrowed(self);
                continue;
            }
            let allowance = preparation.room();
            let temporary = preparation.reserve(allowance)?;
            let options = HistoryOptions {
                cancel: Some(preparation.control().cancel.flag()),
                deadline: preparation.control().deadline,
            };
            let limit = ReadLimit {
                options: &options,
                memory: Some(allowance),
                held: 0,
            };
            let result = self.readonly_branch_read_limited(
                id,
                &At::Commit(seq),
                Purpose::Configuration,
                &limit,
            )?;
            drop(temporary);
            match result {
                OriginRead::Resident(store) => source = Source::Resident(store),
                OriginRead::Inherited(parent) => source = Source::Cold(parent.branch_id),
                OriginRead::Configuration {
                    binding,
                    first_bound_own,
                } => {
                    return self.authenticate_configuration_binding(
                        id,
                        seq,
                        binding,
                        first_bound_own,
                        preparation,
                    );
                }
                _ => {
                    return Err(Error::Corrupt(
                        "configuration lookup returned an RDF view".into(),
                    ));
                }
            }
        }
        Err(managed_history::unavailable(
            "configuration origin has no acyclic retained source",
        ))
    }
}

#[cfg(test)]
mod tests;
