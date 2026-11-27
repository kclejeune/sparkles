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
///
/// ```compile_fail
/// use sparkles_core::store::{BoundEvidenceView, Snapshot};
/// fn escape(view: &BoundEvidenceView) -> Snapshot {
///     view.snapshot().clone()
/// }
/// ```
///
/// ```compile_fail
/// use sparkles_core::store::{BoundEvidenceView, Snapshot};
/// use std::sync::Arc;
/// fn escape(view: &BoundEvidenceView) -> Arc<Snapshot> {
///     view.shared_snapshot().clone()
/// }
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
    // Declared after `_held` and `document` so selection reservations release
    // before the last `Preparation` handle drops.
    preparation: Preparation,
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
    #[cfg(test)]
    pub(crate) fn test_source_bytes(&self) -> u64 {
        self.inner._held.bytes()
    }

    /// The configured execution memory limit of the [`Preparation`] backing this evidence view.
    pub fn preparation_limit(&self) -> u64 {
        self.inner.preparation.limit()
    }

    /// The configured selection memory ceiling of the [`Preparation`] backing this evidence view.
    pub fn selection_limit(&self) -> u64 {
        self.inner.preparation.selection_limit()
    }

    /// Remaining local room in the execution allowance of the [`Preparation`] backing this view.
    pub fn available_execution_bytes(&self) -> u64 {
        self.inner.preparation.available_bytes()
    }

    /// Retain the genuine RDF/configuration source and its original admission.
    /// This owner grants no current authorization or independent output credit.
    pub fn read_source(&self) -> super::BoundReadSource {
        super::BoundReadSource::evidence(self.clone())
    }

    pub(super) fn query_input(&self) -> (&Arc<Snapshot>, &Preparation) {
        (&self.inner.snapshot, &self.inner.preparation)
    }

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

    /// Physical RDF only, restricted to crate internals so callers cannot clone
    /// an uncredited snapshot past this view's reservation.
    pub(crate) fn snapshot(&self) -> &Snapshot {
        &self.inner.snapshot
    }

    /// Shared physical snapshot, restricted to crate internals. External callers
    /// consume this view through [`Self::read_source`], [`Self::prepare_view`], or
    /// [`crate::evidence::write::Reads::from_evidence`].
    pub(crate) fn shared_snapshot(&self) -> &Arc<Snapshot> {
        &self.inner.snapshot
    }

    /// Refuse applying an access policy whose top-level or nested guards were bound
    /// against a different snapshot than this evidence view.
    pub fn validate_access(&self, access: &crate::access::GraphAccess) -> Result<()> {
        access.check_bound_snapshot(&self.inner.snapshot)
    }

    /// Prepare a controlled access view over this evidence view under `preparation`.
    /// The returned [`crate::access::PreparedView`] retains this evidence view and its
    /// original reservation alongside the mask's own credit, and records a weak identity
    /// of `policy` for strict cursor execution (`sparql::select_source_cursor`).
    pub fn prepare_view(
        &self,
        policy: Arc<crate::access::GraphAccess>,
        preparation: &Preparation,
    ) -> Result<crate::access::PreparedView> {
        crate::access::triples::controlled::prepare_evidence(self.clone(), policy, preparation)
    }

    /// Like [`Self::prepare_view`], but prepares a read-only view without binding
    /// `policy.write` guards or invoking write guard providers.
    pub fn prepare_read_only_view(
        &self,
        policy: Arc<crate::access::GraphAccess>,
        preparation: &Preparation,
    ) -> Result<crate::access::PreparedView> {
        crate::access::triples::controlled::prepare_evidence_read_only(
            self.clone(),
            policy,
            preparation,
        )
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

/// The refusal of a historical evidence read on a store without activated
/// configuration history.
fn inactive() -> Error {
    crate::managed::ManagedError::error(
        crate::managed::ManagedErrorKind::ConfigurationHistoryInactive,
        "configuration history is not activated, so evidence views cannot select their configuration; activate it with the closed-store migration",
    )
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

enum ConfigurationSource<'a> {
    Borrowed(&'a Store),
    Resident(Arc<Store>),
    Cold(uuid::Uuid),
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
        let selection = preparation.selection_scope();
        let pair = self.select_configuration_pair(store, at, &selection)?;
        self.finish_evidence_view(pair, &selection, preparation)
    }

    /// Resolve the original publication view of `(receipt_branch, commit)` in `logical`'s
    /// retained first-parent lineage up to `logical_head`.
    ///
    /// Unlike [`Self::evidence_view_with_branch`], when `logical` (or an intermediate
    /// ancestor) was forked at `commit` after a configuration change, this walks retained
    /// origin metadata to the branch that originally authored `commit`, verifies that
    /// authoring branch equals `receipt_branch`, and binds `commit`'s original publication
    /// configuration on that branch (supporting `main`, resident non-`main` branches, and
    /// cold or retired persistent ancestor branches).
    pub(crate) fn publication_evidence_view(
        &self,
        logical: &Store,
        logical_head: u64,
        receipt_branch: uuid::Uuid,
        commit: u64,
        preparation: &Preparation,
    ) -> Result<BoundEvidenceView> {
        let selection = preparation.selection_scope();
        selection.check()?;
        if self.is_branch() || !Arc::ptr_eq(&self.family, &logical.family) {
            return Err(Error::invalid(
                "evidence selection uses the actual owning main Store",
            ));
        }
        if commit > logical_head {
            return Err(managed_history::unavailable(
                "publication commit is ahead of the pinned branch head",
            ));
        }
        let owner = if logical.branch_id() == self.dataset_id {
            crate::branch::CommitRef {
                branch_id: self.dataset_id,
                seq: commit,
            }
        } else {
            let options = HistoryOptions {
                cancel: Some(selection.control().cancel.flag()),
                deadline: selection.control().deadline,
            };
            let limit = ReadLimit {
                options: &options,
                memory: Some(selection.available_bytes()),
                held: 0,
            };
            self.owned_set()?.normalize_with(
                crate::branch::CommitRef {
                    branch_id: logical.branch_id(),
                    seq: commit,
                },
                &limit,
            )?
        };
        if owner.branch_id != receipt_branch {
            return Err(managed_history::unavailable(
                "publication commit does not belong to the named branch in this branch's lineage",
            ));
        }
        let at = At::Commit(commit);
        if owner.branch_id == logical.branch_id() {
            return self.evidence_view_with_branch(logical, &at, preparation);
        }
        if owner.branch_id == self.dataset_id {
            return self.evidence_view_with_branch(self, &at, preparation);
        }
        let set = self.owned_set()?;
        let resident = selection.lock(&set.stores)?.get(&owner.branch_id).cloned();
        if let Some(ancestor) = resident {
            return self.evidence_view_with_branch(&ancestor, &at, preparation);
        }
        let pair =
            self.select_cold_commit_configuration_pair(owner.branch_id, commit, &selection)?;
        self.finish_evidence_view(pair, &selection, preparation)
    }

    fn finish_evidence_view(
        &self,
        pair: SelectedPair,
        selection: &Preparation,
        preparation: &Preparation,
    ) -> Result<BoundEvidenceView> {
        let configuration = pair.semantic.configuration().ok_or_else(|| {
            managed_history::unavailable("historical evidence configuration is explicitly unbound")
        })?;
        let installed = match pair.state.as_ref() {
            ManagedState::Installed(installed) => installed,
            ManagedState::Unavailable { .. } => unreachable!("selection checked installation"),
        };
        let history = installed
            .configuration_history
            .as_ref()
            .ok_or_else(inactive)?;
        let document = if let Some(root) = self.managed_root() {
            managed_history::resolve(
                &root,
                self.owner_dataset_id(),
                history,
                configuration,
                selection,
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
            managed_history::resolve_memory(published, configuration, selection)?
        };
        selection.check()?;
        let (pinned, pin_held) =
            crate::access::triples::evidence::ProjectionConfig::from_generation(
                configuration,
                &document.value.owners,
                &document.value.owner_settings,
                selection,
            )?;
        let mut held = pair.held;
        held.merge(pin_held);
        let snapshot = Arc::new(Snapshot {
            pinned_evidence: Some(pinned),
            ..(*pair.snapshot).clone()
        });
        Ok(BoundEvidenceView {
            inner: Arc::new(BoundInner {
                family: Arc::downgrade(&self.family),
                dataset_id: self.owner_dataset_id(),
                logical_branch: pair.logical_branch,
                serving_branch: pair.serving_branch,
                snapshot,
                commit: pair.commit,
                requested: pair.requested,
                selected_by: pair.selected_by,
                configuration,
                original_data_binding: pair.data,
                document,
                _held: held,
                preparation: preparation.without_selection_scope(),
            }),
        })
    }

    fn select_cold_commit_configuration_pair(
        &self,
        branch_id: uuid::Uuid,
        commit: u64,
        preparation: &Preparation,
    ) -> Result<SelectedPair> {
        let preparation = &preparation.selection_scope();
        preparation.check()?;
        let at = At::Commit(commit);
        let metadata = preparation.reserve(1024)?;
        let before = self.managed_state_shared().ok_or_else(inactive)?;
        let installed = match before.as_ref() {
            ManagedState::Installed(installed) => installed,
            ManagedState::Unavailable { reason, .. } => {
                return Err(managed_history::unavailable(reason.clone()));
            }
        };
        let _current = installed
            .configuration_history
            .as_ref()
            .ok_or_else(inactive)?
            .identity(self.owner_dataset_id());
        let (allowance, mut held) = preparation.reserve_available()?;
        let options = HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        let (snapshot, resolved, serving_branch, charge) =
            self.snapshot_of_origin_branch_limited_charged(branch_id, &at, &options, allowance)?;
        let limit = ReadLimit {
            options: &options,
            memory: Some(allowance),
            held: 0,
        };
        let retained = charge
            .delta_bytes
            .checked_add(snapshot.generation.retained_bytes_with(&limit)?)
            .and_then(|bytes| bytes.checked_add(1024))
            .ok_or_else(|| preparation.exhausted())?;
        if retained > allowance {
            return Err(preparation.exhausted());
        }
        held.release_to(retained);
        held.merge(metadata);
        let data =
            self.configuration_of_cold_branch(branch_id, resolved.commit.seq, preparation)?;
        let after = self.managed_state_shared();
        if !after
            .as_ref()
            .is_some_and(|after| Arc::ptr_eq(after, &before))
        {
            return Err(managed_history::unavailable(
                "configuration changed during evidence selection",
            ));
        }
        preparation.check()?;
        Ok(SelectedPair {
            snapshot,
            commit: resolved.commit,
            requested: at.clone(),
            selected_by: at,
            logical_branch: branch_id,
            serving_branch,
            semantic: data,
            data,
            state: before,
            held,
        })
    }

    pub(super) fn select_configuration_pair(
        &self,
        store: &Store,
        at: &At,
        preparation: &Preparation,
    ) -> Result<SelectedPair> {
        let preparation = &preparation.selection_scope();
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
        let before = self.managed_state_shared().ok_or_else(inactive)?;
        let installed = match before.as_ref() {
            ManagedState::Installed(installed) => installed,
            ManagedState::Unavailable { reason, .. } => {
                return Err(managed_history::unavailable(reason.clone()));
            }
        };
        let current = installed
            .configuration_history
            .as_ref()
            .ok_or_else(inactive)?
            .identity(self.owner_dataset_id());
        let (allowance, mut held) = preparation.reserve_available()?;
        let options = HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        let (snapshot, resolved, serving_branch, charge) =
            self.snapshot_at_with_branch_limited_charged(store, at, &options, allowance)?;
        let limit = ReadLimit {
            options: &options,
            memory: Some(allowance),
            held: 0,
        };
        let retained = charge
            .delta_bytes
            .checked_add(snapshot.generation.retained_bytes_with(&limit)?)
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
        self.configuration_of_source(ConfigurationSource::Borrowed(store), seq, preparation)
    }

    fn configuration_of_cold_branch(
        &self,
        branch_id: uuid::Uuid,
        seq: u64,
        preparation: &Preparation,
    ) -> Result<ConfigurationBinding> {
        self.configuration_of_source(ConfigurationSource::Cold(branch_id), seq, preparation)
    }

    fn configuration_of_source<'a>(
        &'a self,
        mut source: ConfigurationSource<'a>,
        seq: u64,
        preparation: &Preparation,
    ) -> Result<ConfigurationBinding> {
        use branching::readonly::{OriginRead, Purpose};
        for _ in 0..1024 {
            preparation.check()?;
            let resident = match &source {
                ConfigurationSource::Borrowed(store) => Some(*store),
                ConfigurationSource::Resident(store) => Some(store.as_ref()),
                ConfigurationSource::Cold(_) => None,
            };
            if let Some(store) = resident {
                if let Some(identity) = &store.branching.ident
                    && seq < identity.from.seq
                {
                    // Do not normalize through intermediate branch roots: each
                    // one's own primary origin must win at its exact sequence.
                    source = ConfigurationSource::Cold(identity.from.branch_id);
                    continue;
                }
                return store.configuration_binding_at(seq, preparation);
            }
            let ConfigurationSource::Cold(id) = source else {
                unreachable!()
            };
            if id == self.branch_id() {
                source = ConfigurationSource::Borrowed(self);
                continue;
            }
            let (allowance, temporary) = preparation.reserve_available()?;
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
                OriginRead::Resident(store) => source = ConfigurationSource::Resident(store),
                OriginRead::Inherited(parent) => source = ConfigurationSource::Cold(parent.branch_id),
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

    /// Pin a fail-closed [`EvidenceConfig::Unavailable`] onto `snapshot`, charging the
    /// cloned snapshot header to `preparation`.
    fn pin_unavailable(
        snapshot: Arc<Snapshot>,
        reason: String,
        preparation: &Preparation,
    ) -> Result<(Arc<Snapshot>, Reservation)> {
        let bytes = (reason.capacity() as u64)
            .saturating_add(crate::access::triples::evidence::PINNED_SNAPSHOT_HEADER_BYTES);
        let held = preparation.reserve(bytes)?;
        let pinned = Arc::new(Snapshot {
            pinned_evidence: Some(
                crate::access::triples::evidence::EvidenceConfig::Unavailable(reason),
            ),
            ..(*snapshot).clone()
        });
        Ok((pinned, held))
    }

    /// When configuration history is activated and `at` selects a historical state or
    /// named snapshot, pin the resolved historical evidence visibility configuration onto
    /// `snapshot` so support-aware projection visibility uses the exact policies and
    /// protection nonces recorded at that state rather than current live settings.
    ///
    /// Fails closed by pinning [`EvidenceConfig::Unavailable`] whenever the managed
    /// registry is unavailable, the named pin changed/expired, or an authenticated
    /// historical configuration generation cannot be resolved.
    pub(super) fn pin_historical_evidence(
        &self,
        store: &Store,
        at: &At,
        seq: u64,
        snapshot: Arc<Snapshot>,
        preparation: &Preparation,
    ) -> Result<(Arc<Snapshot>, Reservation)> {
        if matches!(at, At::Head) || !self.configuration_activated() {
            return Ok((snapshot, preparation.reserve(0)?));
        }
        let Some(state) = self.managed_state_shared() else {
            return Ok((snapshot, preparation.reserve(0)?));
        };
        let installed = match state.as_ref() {
            ManagedState::Installed(installed) => installed,
            ManagedState::Unavailable { reason, .. } => {
                return Self::pin_unavailable(snapshot, reason.clone(), preparation);
            }
        };
        let Some(history) = installed.configuration_history.as_ref() else {
            return Ok((snapshot, preparation.reserve(0)?));
        };
        let binding = match at {
            At::Snapshot(name) => {
                let pinned = if let Some(h) = &store.history {
                    let h = preparation.lock(h)?;
                    h.pins
                        .get(name)
                        .map(|pin| (pin.seq, pin.expires_ms, pin.configuration.clone()))
                } else if let Some(h) = &store.mem_history {
                    let h = preparation.lock(h)?;
                    h.pins
                        .get(name)
                        .map(|(pin, _)| (pin.seq, pin.expires_ms, pin.configuration.clone()))
                } else {
                    None
                };
                let Some((pin_seq, expires_ms, cfg)) = pinned else {
                    return Self::pin_unavailable(
                        snapshot,
                        "named snapshot is no longer retained during configuration selection"
                            .into(),
                        preparation,
                    );
                };
                if pin_seq != seq || expires_ms.is_some_and(|expiry| expiry <= store.now_ms()) {
                    return Self::pin_unavailable(
                        snapshot,
                        "named snapshot changed or expired during configuration selection".into(),
                        preparation,
                    );
                }
                match cfg {
                    Some(cfg) => {
                        if let Err(e) = cfg.validate(self.owner_dataset_id()) {
                            return Self::pin_unavailable(
                                snapshot,
                                format!("named snapshot configuration is invalid: {e}"),
                                preparation,
                            );
                        }
                        cfg.binding
                    }
                    None => ConfigurationBinding::LegacyUnbound,
                }
            }
            At::Commit(_) | At::Time(_) => {
                let result = if !self.is_branch() {
                    self.configuration_of_branch(store, seq, preparation)
                } else {
                    store.configuration_binding_at(seq, preparation)
                };
                match result {
                    Ok(b) => b,
                    Err(e @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                        return Err(e);
                    }
                    Err(e) => {
                        return Self::pin_unavailable(
                            snapshot,
                            format!("historical configuration binding cannot be resolved: {e}"),
                            preparation,
                        );
                    }
                }
            }
            At::Head => return Ok((snapshot, preparation.reserve(0)?)),
        };
        self.pin_resolved_binding(installed, history, binding, snapshot, preparation)
    }

    /// Pin historical evidence for a branch identified by `branch_id` (resident or cold).
    pub(super) fn pin_historical_evidence_for_branch(
        &self,
        branch_id: uuid::Uuid,
        at: &At,
        seq: u64,
        snapshot: Arc<Snapshot>,
        preparation: &Preparation,
    ) -> Result<(Arc<Snapshot>, Reservation)> {
        if matches!(at, At::Head) || !self.configuration_activated() {
            return Ok((snapshot, preparation.reserve(0)?));
        }
        if branch_id == self.branch_id() {
            return self.pin_historical_evidence(self, at, seq, snapshot, preparation);
        }
        let Some(state) = self.managed_state_shared() else {
            return Ok((snapshot, preparation.reserve(0)?));
        };
        let installed = match state.as_ref() {
            ManagedState::Installed(installed) => installed,
            ManagedState::Unavailable { reason, .. } => {
                return Self::pin_unavailable(snapshot, reason.clone(), preparation);
            }
        };
        let Some(history) = installed.configuration_history.as_ref() else {
            return Ok((snapshot, preparation.reserve(0)?));
        };
        if let At::Snapshot(_) = at {
            // Check already-resident branch stores without opening cold branches.
            if let Ok(set) = self.owned_set()
                && let Some(branch_store) = preparation.lock(&set.stores)?.get(&branch_id).cloned()
            {
                return self.pin_historical_evidence(&branch_store, at, seq, snapshot, preparation);
            }
            let (allowance, temporary) = preparation.reserve_available()?;
            let options = HistoryOptions {
                cancel: Some(preparation.control().cancel.flag()),
                deadline: preparation.control().deadline,
            };
            let limit = ReadLimit {
                options: &options,
                memory: Some(allowance),
                held: 0,
            };
            let pin_read = self.readonly_branch_read_limited(
                branch_id,
                at,
                branching::readonly::Purpose::Pin,
                &limit,
            );
            drop(temporary);
            match pin_read {
                Ok(branching::readonly::OriginRead::Resident(branch_store)) => {
                    return self.pin_historical_evidence(
                        &branch_store,
                        at,
                        seq,
                        snapshot,
                        preparation,
                    );
                }
                Ok(branching::readonly::OriginRead::Pin {
                    seq: pin_seq,
                    expires_ms,
                    configuration,
                }) => {
                    if pin_seq != seq || expires_ms.is_some_and(|e| e <= self.now_ms()) {
                        return Self::pin_unavailable(
                            snapshot,
                            "cold branch named snapshot changed or expired during configuration selection"
                                .into(),
                            preparation,
                        );
                    }
                    let binding = match configuration {
                        Some(cfg) => {
                            if let Err(e) = cfg.validate(self.owner_dataset_id()) {
                                return Self::pin_unavailable(
                                    snapshot,
                                    format!(
                                        "cold branch named snapshot configuration is invalid: {e}"
                                    ),
                                    preparation,
                                );
                            }
                            cfg.binding
                        }
                        None => ConfigurationBinding::LegacyUnbound,
                    };
                    return self.pin_resolved_binding(
                        installed,
                        history,
                        binding,
                        snapshot,
                        preparation,
                    );
                }
                Err(e @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                    return Err(e);
                }
                Err(e) => {
                    return Self::pin_unavailable(
                        snapshot,
                        format!("cold branch named snapshot configuration cannot be verified: {e}"),
                        preparation,
                    );
                }
                Ok(_) => {
                    return Self::pin_unavailable(
                        snapshot,
                        "cold branch named snapshot configuration lookup returned unexpected state"
                            .into(),
                        preparation,
                    );
                }
            }
        }
        let binding = match self.configuration_of_cold_branch(branch_id, seq, preparation) {
            Ok(b) => b,
            Err(e @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                return Err(e);
            }
            Err(e) => {
                return Self::pin_unavailable(
                    snapshot,
                    format!("cold branch historical configuration binding cannot be resolved: {e}"),
                    preparation,
                );
            }
        };
        self.pin_resolved_binding(installed, history, binding, snapshot, preparation)
    }

    fn pin_resolved_binding(
        &self,
        installed: &crate::managed::Installed,
        history: &managed_history::Root,
        binding: ConfigurationBinding,
        snapshot: Arc<Snapshot>,
        preparation: &Preparation,
    ) -> Result<(Arc<Snapshot>, Reservation)> {
        let Some(configuration) = binding.configuration() else {
            // C25: activation does not certify pre-activation commits or legacy pins
            // from mutable current settings; fail closed for restricted readers.
            return Self::pin_unavailable(
                snapshot,
                "historical state has no authenticated configuration binding in an activated family"
                    .into(),
                preparation,
            );
        };
        let document = if let Some(root) = self.managed_root() {
            managed_history::resolve(
                &root,
                self.owner_dataset_id(),
                history,
                configuration,
                preparation,
            )
        } else if let Some(published) = installed.history_memory.as_ref() {
            if published.prepared.identity == history.identity(self.owner_dataset_id()) {
                managed_history::resolve_memory(published, configuration, preparation)
            } else {
                return Self::pin_unavailable(
                    snapshot,
                    "historical configuration memory root differs".into(),
                    preparation,
                );
            }
        } else {
            return Self::pin_unavailable(
                snapshot,
                "historical configuration document storage is unavailable".into(),
                preparation,
            );
        };
        let document = match document {
            Ok(doc) => doc,
            Err(e @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                return Err(e);
            }
            Err(e) => {
                return Self::pin_unavailable(
                    snapshot,
                    format!("historical configuration document cannot be resolved: {e}"),
                    preparation,
                );
            }
        };
        let (pinned, held) = crate::access::triples::evidence::ProjectionConfig::from_generation(
            configuration,
            &document.value.owners,
            &document.value.owner_settings,
            preparation,
        )?;
        Ok((
            Arc::new(Snapshot {
                pinned_evidence: Some(pinned),
                ..(*snapshot).clone()
            }),
            held,
        ))
    }
}

/// Handle from a [`Snapshot`] back to its branch's retained history. Resident stores use
/// a non-owning [`std::sync::Weak`] reference; cold read-only snapshots own a lightweight
/// [`Arc`] that holds only paths and shared caches (no `Family`, `Store`, or process lock).
#[derive(Clone)]
pub(crate) enum HistoryOriginHandle {
    Resident(std::sync::Weak<HistoryOrigin>),
    Cold(Arc<HistoryOrigin>),
}

impl HistoryOriginHandle {
    pub(crate) fn upgrade(&self) -> Option<Arc<HistoryOrigin>> {
        match self {
            Self::Resident(w) => w.upgrade(),
            Self::Cold(a) => Some(a.clone()),
        }
    }
}

/// Non-owning handle owned by [`Store`] (or lightweight cold reader owned by cold
/// snapshots) so support-aware visibility can verify earlier-commit same-branch
/// `ev:StoredFact` references against retained history without retaining `Family` or its
/// process lock.
pub(crate) struct HistoryOrigin {
    dataset_id: uuid::Uuid,
    branch_id: uuid::Uuid,
    is_cold: bool,
    root: Option<PathBuf>,
    managed_root: Option<PathBuf>,
    history: Option<Arc<Mutex<crate::history::HistoryState>>>,
    mem_history: Option<Arc<Mutex<crate::history::MemHistory>>>,
    managed: Arc<super::family::ManagedSlot>,
    current: std::sync::Weak<arc_swap::ArcSwap<Snapshot>>,
    cache: Option<Arc<BlockCache>>,
    results: Option<Arc<crate::sparql::cache::ResultCache>>,
    union_default_graph: bool,
    aggregate_errors: crate::sparql::AggregateErrors,
    geo_op_vertices: u64,
    history_cache_bytes: u64,
}

impl HistoryOrigin {
    pub(crate) fn new_memory(
        dataset_id: uuid::Uuid,
        branch_id: uuid::Uuid,
        mem_history: Option<Arc<Mutex<crate::history::MemHistory>>>,
        managed: Arc<super::family::ManagedSlot>,
        current: std::sync::Weak<arc_swap::ArcSwap<Snapshot>>,
    ) -> Self {
        Self {
            dataset_id,
            branch_id,
            is_cold: false,
            root: None,
            managed_root: None,
            history: None,
            mem_history,
            managed,
            current,
            cache: None,
            results: None,
            union_default_graph: false,
            aggregate_errors: Default::default(),
            geo_op_vertices: 0,
            history_cache_bytes: 0,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_persistent(
        dataset_id: uuid::Uuid,
        branch_id: uuid::Uuid,
        root: PathBuf,
        managed_root: PathBuf,
        history: Option<Arc<Mutex<crate::history::HistoryState>>>,
        managed: Arc<super::family::ManagedSlot>,
        current: std::sync::Weak<arc_swap::ArcSwap<Snapshot>>,
        cache: Arc<BlockCache>,
        results: Arc<crate::sparql::cache::ResultCache>,
        union_default_graph: bool,
        aggregate_errors: crate::sparql::AggregateErrors,
        geo_op_vertices: u64,
        history_cache_bytes: u64,
    ) -> Self {
        Self {
            dataset_id,
            branch_id,
            is_cold: false,
            root: Some(root),
            managed_root: Some(managed_root),
            history,
            mem_history: None,
            managed,
            current,
            cache: Some(cache),
            results: Some(results),
            union_default_graph,
            aggregate_errors,
            geo_op_vertices,
            history_cache_bytes,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_cold(
        dataset_id: uuid::Uuid,
        branch_id: uuid::Uuid,
        root: PathBuf,
        managed_root: PathBuf,
        managed: Arc<super::family::ManagedSlot>,
        cache: Arc<BlockCache>,
        results: Arc<crate::sparql::cache::ResultCache>,
        union_default_graph: bool,
        aggregate_errors: crate::sparql::AggregateErrors,
        geo_op_vertices: u64,
        history_cache_bytes: u64,
    ) -> Self {
        Self {
            dataset_id,
            branch_id,
            is_cold: true,
            root: Some(root),
            managed_root: Some(managed_root),
            history: None,
            mem_history: None,
            managed,
            current: std::sync::Weak::new(),
            cache: Some(cache),
            results: Some(results),
            union_default_graph,
            aggregate_errors,
            geo_op_vertices,
            history_cache_bytes,
        }
    }

    #[allow(clippy::type_complexity)]
    pub(super) fn cold_parts(
        &self,
    ) -> (
        uuid::Uuid,
        uuid::Uuid,
        &Path,
        &Arc<BlockCache>,
        &Arc<crate::sparql::cache::ResultCache>,
        &Arc<super::family::ManagedSlot>,
        bool,
        crate::sparql::AggregateErrors,
        u64,
        u64,
    ) {
        (
            self.dataset_id,
            self.branch_id,
            self.root.as_deref().expect("cold origin has root"),
            self.cache.as_ref().expect("cold origin has cache"),
            self.results.as_ref().expect("cold origin has results"),
            &self.managed,
            self.union_default_graph,
            self.aggregate_errors,
            self.geo_op_vertices,
            self.history_cache_bytes,
        )
    }

    /// Resolve retained commit `seq` on this branch within `preparation`'s controls and
    /// memory budget, pinning its historical evidence configuration when configuration
    /// history is activated. Returns `Ok(None)` when the commit is no longer retained.
    pub(crate) fn snapshot_at_commit(
        self: &Arc<Self>,
        seq: u64,
        preparation: &Preparation,
    ) -> Result<Option<(Arc<Snapshot>, Reservation)>> {
        preparation.check()?;
        if self.is_cold {
            return branching::readonly::cold_commit_snapshot_limited(self, seq, preparation);
        }
        let Some(current_swap) = self.current.upgrade() else {
            return Ok(None);
        };
        let live = current_swap.load_full();
        if let Some(mem) = &self.mem_history {
            let m = preparation.lock(mem)?;
            let snap = if seq == live.commit {
                live.clone()
            } else {
                let Some(s) = m.get(seq) else {
                    return Ok(None);
                };
                s
            };
            let binding = m.configurations.get(&seq).copied().unwrap_or_default();
            drop(m);
            let delta_bytes = history_limited::preparation_delta_bytes(&snap.delta)
                .max(history_limited::SHARED_SNAPSHOT_BYTES);
            let mut held = preparation.reserve(delta_bytes)?;
            let (snap, pin_held) = self.pin_commit_binding(binding, snap, preparation)?;
            held.merge(pin_held);
            return Ok(Some((snap, held)));
        }
        let (Some(root), Some(hist), Some(cache), Some(results)) =
            (&self.root, &self.history, &self.cache, &self.results)
        else {
            return Ok(None);
        };
        let current_gen = commit::generation_number(&live.generation.name);
        let head_seq = live.commit;
        let options = HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        let (allowance, mut temp_guard) = preparation.reserve_available()?;
        let mem_cap = allowance.min(
            self.history_cache_bytes
                .max(history_limited::SHARED_SNAPSHOT_BYTES),
        );
        let limit = ReadLimit {
            options: &options,
            memory: Some(mem_cap),
            held: crate::history::READ_METADATA_BYTES,
        };
        let h = limit.lock(hist)?;
        let Some(owner) = h.owner_with(seq, current_gen, head_seq, &limit)? else {
            return Ok(None);
        };
        let cached = h
            .cache
            .iter()
            .find(|e| e.generation == owner && e.seq == seq)
            .map(|e| e.snap.clone());
        let entry = h.gens.get(&owner).cloned();
        drop(h);
        let Some(entry) = entry else {
            return Ok(None);
        };
        let (snap, retained_bytes) = if let Some(cached) = cached {
            let gen_bytes = if Arc::ptr_eq(&cached.generation, &live.generation) {
                0
            } else {
                cached.generation.retained_bytes_limited(&limit)?
            };
            (
                cached,
                history_limited::SHARED_SNAPSHOT_BYTES.saturating_add(gen_bytes),
            )
        } else if seq == live.commit {
            (live.clone(), history_limited::SHARED_SNAPSHOT_BYTES)
        } else {
            let mut budget = cold::ColdBudget::new(limit)?;
            let (generation, gen_bytes) = if owner == current_gen {
                (live.generation.clone(), 0u64)
            } else {
                let loaded = match cold::read_link_controlled(&entry.dir, &mut budget)? {
                    Some(file) => Generation::open_linked_controlled(
                        &entry.dir,
                        &entry.name,
                        root,
                        file,
                        cache,
                        &mut budget,
                    )?,
                    None => Generation::open_sealed_controlled(
                        &entry.dir,
                        &entry.name,
                        None,
                        &mut budget,
                    )?,
                };
                (Arc::new(loaded.value), loaded.charge)
            };
            let seed = generation.link.as_ref().map(|link| &link.base_delta);
            let mut delta = seed.cloned().unwrap_or_default();
            if seq > entry.base.seq {
                let wal_path = entry.dir.join("wal.log");
                let from = wal::WalPoint {
                    seq: entry.base.seq,
                    offset: 0,
                };
                // Scope the boundary-finding cursor so its BufReader and transaction
                // buffer are released before `apply_forward` opens its replay cursor.
                let target = {
                    let cursor_limit = budget.owned_limit()?;
                    let mut cursor = wal::WalCursor::open_with(&wal_path, from, &cursor_limit, 0)?;
                    let mut target = None;
                    while let Some((found, _, _)) = cursor.next_with(&cursor_limit, 0)? {
                        if found == seq {
                            target = Some(cursor.position());
                            break;
                        }
                        if found > seq {
                            break;
                        }
                    }
                    target
                };
                let Some(target) = target else {
                    return Ok(None);
                };
                wal::apply_forward(
                    &wal_path,
                    &generation,
                    cache,
                    &mut delta,
                    from,
                    target,
                    &budget.owned_limit()?,
                )?;
            }
            let delta_bytes = history_limited::preparation_delta_bytes(&delta)
                .max(history_limited::SHARED_SNAPSHOT_BYTES);
            let dvocab_len = generation.dvocab.len_limited(&limit)?;
            let built = Arc::new(
                SnapshotBuilder::of_generation(
                    snapshot::Shared {
                        dataset_id: self.dataset_id,
                        branch_id: self.branch_id,
                        cache,
                        results,
                        union_default_graph: self.union_default_graph,
                        aggregate_errors: self.aggregate_errors,
                        geo_op_vertices: self.geo_op_vertices,
                        change_log: None,
                        managed: Some(self.managed.clone()),
                        history_origin: Some(HistoryOriginHandle::Resident(Arc::downgrade(self))),
                    },
                    generation,
                    seq,
                )
                .delta(delta)
                .dvocab_len(dvocab_len)
                .historical(true)
                .build(),
            );
            (built, delta_bytes.saturating_add(gen_bytes))
        };
        if retained_bytes > allowance {
            return Err(preparation.exhausted());
        }
        temp_guard.release_to(retained_bytes);
        // Resolve historical configuration binding for persistent commit when activated.
        let binding = self.persistent_binding_for(&entry, seq, preparation)?;
        let (snap, pin_held) = self.pin_commit_binding(binding, snap, preparation)?;
        temp_guard.merge(pin_held);
        Ok(Some((snap, temp_guard)))
    }

    fn persistent_binding_for(
        &self,
        entry: &crate::history::GenEntry,
        seq: u64,
        preparation: &Preparation,
    ) -> Result<ConfigurationBinding> {
        let state = self.managed.load_full();
        let Some(ManagedState::Installed(installed)) = state.as_deref() else {
            return Ok(ConfigurationBinding::LegacyUnbound);
        };
        let Some(history) = installed.configuration_history.as_ref() else {
            return Ok(ConfigurationBinding::LegacyUnbound);
        };
        let (allowance, _temp) = preparation.reserve_available()?;
        let options = HistoryOptions {
            cancel: Some(preparation.control().cancel.flag()),
            deadline: preparation.control().deadline,
        };
        let limit = ReadLimit {
            options: &options,
            memory: Some(allowance),
            held: 0,
        };
        let mut budget = cold::ColdBudget::new(limit)?;
        let physical =
            commit::read_gen_commit_controlled(&entry.dir, &mut budget)?.ok_or_else(|| {
                managed_history::unavailable("retained generation has no commit identity")
            })?;
        if physical.value.commit.seq == seq {
            return Ok(physical.value.configuration);
        }
        let from = wal::WalPoint {
            seq: entry.base.seq,
            offset: 0,
        };
        let cursor_limit = budget.owned_limit()?;
        let mut cursor =
            wal::WalCursor::open_with(&entry.dir.join("wal.log"), from, &cursor_limit, 0)?;
        while let Some((found, _, _)) = cursor.next_with(&cursor_limit, 0)? {
            if found > seq {
                break;
            }
            if found == seq {
                return Ok(match cursor.configuration() {
                    Some(generation) => managed_history::binding(ConfigurationIdentity {
                        dataset_id: self.dataset_id,
                        lineage_id: history.lineage_id,
                        generation,
                    }),
                    None => ConfigurationBinding::LegacyUnbound,
                });
            }
        }
        Err(managed_history::unavailable(
            "retained WAL has no selected binding",
        ))
    }

    pub(super) fn pin_commit_binding(
        &self,
        binding: ConfigurationBinding,
        snapshot: Arc<Snapshot>,
        preparation: &Preparation,
    ) -> Result<(Arc<Snapshot>, Reservation)> {
        let state = self.managed.load_full();
        let Some(state) = state else {
            return Ok((snapshot, preparation.reserve(0)?));
        };
        let installed = match state.as_ref() {
            ManagedState::Installed(installed) => installed,
            ManagedState::Unavailable { reason, .. } => {
                return Store::pin_unavailable(snapshot, reason.clone(), preparation);
            }
        };
        let Some(history) = installed.configuration_history.as_ref() else {
            return Ok((snapshot, preparation.reserve(0)?));
        };
        let Some(configuration) = binding.configuration() else {
            // C25: activation does not certify pre-activation commits from live settings;
            // fail closed for restricted readers.
            return Store::pin_unavailable(
                snapshot,
                "historical commit has no authenticated configuration binding in an activated family"
                    .into(),
                preparation,
            );
        };
        let document = if let Some(root) = &self.managed_root {
            managed_history::resolve(root, self.dataset_id, history, configuration, preparation)
        } else if let Some(published) = installed.history_memory.as_ref() {
            if published.prepared.identity == history.identity(self.dataset_id) {
                managed_history::resolve_memory(published, configuration, preparation)
            } else {
                return Store::pin_unavailable(
                    snapshot,
                    "historical configuration memory root differs".into(),
                    preparation,
                );
            }
        } else {
            return Store::pin_unavailable(
                snapshot,
                "historical configuration document storage is unavailable".into(),
                preparation,
            );
        };
        let document = match document {
            Ok(doc) => doc,
            Err(e @ (Error::Cancelled | Error::Timeout | Error::BudgetExceeded(_))) => {
                return Err(e);
            }
            Err(e) => {
                return Store::pin_unavailable(
                    snapshot,
                    format!("historical configuration document cannot be resolved: {e}"),
                    preparation,
                );
            }
        };
        let (pinned, held) = crate::access::triples::evidence::ProjectionConfig::from_generation(
            configuration,
            &document.value.owners,
            &document.value.owner_settings,
            preparation,
        )?;
        Ok((
            Arc::new(Snapshot {
                pinned_evidence: Some(pinned),
                ..(*snapshot).clone()
            }),
            held,
        ))
    }
}

#[cfg(test)]
mod tests;
